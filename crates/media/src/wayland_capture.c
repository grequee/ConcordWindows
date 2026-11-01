/* Native ScreenCast portal + PipeWire bridge. All objects and callbacks live
 * on one capture thread. No pixels or file descriptors enter the WebView.
 * Protocol: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
 */
#include <gio/gio.h>
#include <gio/gunixfdlist.h>
#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/buffer/meta.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

typedef int (*stop_fn)(void *);
typedef void (*ready_fn)(void *);
/* Format values are private to this bridge: 0 BGRA, 1 BGRx, 2 RGBA, 3 RGBx. */
typedef int (*frame_fn)(void *, const uint8_t *, size_t, uint32_t, uint32_t, int32_t, uint32_t);

struct capture {
    void *user;
    stop_fn stopped;
    ready_fn ready;
    frame_fn frame;
    GMainContext *glib_loop;
    GDBusConnection *bus;
    gchar *session;
    gchar *request;
    GVariant *response;
    guint response_code;
    guint response_sub;
    guint closed_sub;
    gboolean closed;
    gboolean ready_sent;
    struct pw_main_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_stream *stream;
    struct spa_hook core_hook;
    struct spa_hook stream_hook;
    struct spa_video_info_raw format;
    char *error;
    size_t error_len;
};

static void fail(struct capture *c, const char *message) {
    if (!c->error[0]) g_strlcpy(c->error, message, c->error_len);
}

static void dbus_fail(struct capture *c, GError *error) {
    fail(c, error ? error->message : "o portal não retornou uma resposta válida");
    g_clear_error(&error);
}

static void close_object(struct capture *c, const char *path, const char *interface) {
    if (!path || !c->bus || g_dbus_connection_is_closed(c->bus)) return;
    GVariant *reply = g_dbus_connection_call_sync(c->bus,
        "org.freedesktop.portal.Desktop", path, interface, "Close", NULL,
        NULL, G_DBUS_CALL_FLAGS_NONE, 1000, NULL, NULL);
    if (reply) g_variant_unref(reply);
}

static void on_response(GDBusConnection *bus, const gchar *sender, const gchar *path,
        const gchar *interface, const gchar *signal, GVariant *parameters, gpointer user) {
    (void)bus; (void)sender; (void)interface; (void)signal;
    struct capture *c = user;
    if (c->request && !strcmp(path, c->request) && !c->response)
        g_variant_get(parameters, "(u@a{sv})", &c->response_code, &c->response);
}

static void on_closed(GDBusConnection *bus, const gchar *sender, const gchar *path,
        const gchar *interface, const gchar *signal, GVariant *parameters, gpointer user) {
    (void)bus; (void)sender; (void)path; (void)interface; (void)signal; (void)parameters;
    struct capture *c = user;
    c->closed = TRUE;
    fail(c, "o compartilhamento foi encerrado pelo portal do desktop");
}

static void pump_portal(struct capture *c) {
    /* Bound dispatch so a busy bus cannot starve PipeWire or cancellation. */
    for (int i = 0; i < 32 && g_main_context_pending(c->glib_loop); i++)
        g_main_context_iteration(c->glib_loop, FALSE);
    if (c->bus && g_dbus_connection_is_closed(c->bus))
        fail(c, "a conexão com o portal do desktop foi encerrada");
}

static gchar *request_options(struct capture *c, GVariantBuilder *options) {
    static gint counter = 0;
    gchar *token = g_strdup_printf("concord_%u", (guint)g_atomic_int_add(&counter, 1));
    const gchar *unique = g_dbus_connection_get_unique_name(c->bus);
    gchar *sender = g_strdup(unique[0] == ':' ? unique + 1 : unique);
    for (gchar *p = sender; *p; p++) if (*p == '.') *p = '_';
    g_clear_pointer(&c->request, g_free);
    c->request = g_strdup_printf("/org/freedesktop/portal/desktop/request/%s/%s", sender, token);
    g_variant_builder_init(options, G_VARIANT_TYPE_VARDICT);
    g_variant_builder_add(options, "{sv}", "handle_token", g_variant_new_string(token));
    g_free(sender);
    return token;
}

/* Subscribe before issuing a method: portals may emit Response immediately. */
static GVariant *portal_request(struct capture *c, const char *method, GVariant *args) {
    GError *error = NULL;
    GVariant *reply = g_dbus_connection_call_sync(c->bus,
        "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast", method, args, G_VARIANT_TYPE("(o)"),
        G_DBUS_CALL_FLAGS_NONE, 5000, NULL, &error);
    if (!reply) { dbus_fail(c, error); return NULL; }
    const gchar *path;
    g_variant_get(reply, "(&o)", &path);
    g_free(c->request);
    c->request = g_strdup(path);
    g_variant_unref(reply);

    gint64 deadline = g_get_monotonic_time() + 120 * G_TIME_SPAN_SECOND;
    while (!c->response && !c->error[0] && !c->stopped(c->user)) {
        pump_portal(c);
        if (g_get_monotonic_time() > deadline) {
            fail(c, "tempo esgotado aguardando a seleção de tela no portal");
            break;
        }
        if (!c->response) g_usleep(10000);
    }
    if (!c->response) {
        close_object(c, c->request, "org.freedesktop.portal.Request");
        if (!c->error[0]) fail(c, "compartilhamento de tela cancelado");
        return NULL;
    }
    GVariant *result = c->response;
    c->response = NULL;
    g_clear_pointer(&c->request, g_free);
    if (c->response_code != 0) {
        fail(c, c->response_code == 1 ? "seleção de tela cancelada no portal"
            : "o portal não autorizou o compartilhamento de tela");
        g_variant_unref(result);
        return NULL;
    }
    return result;
}

static guint portal_property(struct capture *c, const char *name) {
    GVariant *reply = g_dbus_connection_call_sync(c->bus,
        "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.DBus.Properties", "Get",
        g_variant_new("(ss)", "org.freedesktop.portal.ScreenCast", name),
        G_VARIANT_TYPE("(v)"), G_DBUS_CALL_FLAGS_NONE, 5000, NULL, NULL);
    if (!reply) return 0;
    GVariant *value;
    g_variant_get(reply, "(v)", &value);
    guint result = g_variant_is_of_type(value, G_VARIANT_TYPE_UINT32)
        ? g_variant_get_uint32(value) : 0;
    g_variant_unref(value);
    g_variant_unref(reply);
    return result;
}

static int open_portal(struct capture *c, uint32_t *node, char **serial) {
    GError *error = NULL;
    gchar *address = g_dbus_address_get_for_bus_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!address) { dbus_fail(c, error); return -1; }
    c->bus = g_dbus_connection_new_for_address_sync(address,
        G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT | G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,
        NULL, NULL, &error);
    g_free(address);
    if (!c->bus) { dbus_fail(c, error); return -1; }
    g_dbus_connection_set_exit_on_close(c->bus, FALSE);
    c->response_sub = g_dbus_connection_signal_subscribe(c->bus,
        "org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response",
        NULL, NULL, G_DBUS_SIGNAL_FLAGS_NONE, on_response, c, NULL);

    GVariantBuilder options;
    gchar *token = request_options(c, &options);
    g_variant_builder_add(&options, "{sv}", "session_handle_token", g_variant_new_string(token));
    g_free(token);
    GVariant *result = portal_request(c, "CreateSession", g_variant_new("(a{sv})", &options));
    if (!result) return -1;
    const gchar *session = NULL;
    if (!g_variant_lookup(result, "session_handle", "&s", &session))
        g_variant_lookup(result, "session_handle", "&o", &session);
    if (session && g_variant_is_object_path(session)) c->session = g_strdup(session);
    g_variant_unref(result);
    if (!c->session) { fail(c, "o portal não retornou uma sessão de captura"); return -1; }
    c->closed_sub = g_dbus_connection_signal_subscribe(c->bus,
        "org.freedesktop.portal.Desktop", "org.freedesktop.portal.Session", "Closed",
        c->session, NULL, G_DBUS_SIGNAL_FLAGS_NONE, on_closed, c, NULL);

    guint types = portal_property(c, "AvailableSourceTypes") & 3u;
    if (!types) { fail(c, "instale um backend de portal com suporte a ScreenCast (KDE/GNOME/wlr)"); return -1; }
    guint cursor_modes = portal_property(c, "AvailableCursorModes");
    token = request_options(c, &options);
    g_free(token);
    g_variant_builder_add(&options, "{sv}", "types", g_variant_new_uint32(types));
    g_variant_builder_add(&options, "{sv}", "multiple", g_variant_new_boolean(FALSE));
    if (cursor_modes & 2u)
        g_variant_builder_add(&options, "{sv}", "cursor_mode", g_variant_new_uint32(2));
    result = portal_request(c, "SelectSources", g_variant_new("(oa{sv})", c->session, &options));
    if (!result) return -1;
    g_variant_unref(result);

    token = request_options(c, &options);
    g_free(token);
    /* Empty parent is supported by the portal. No remote-control permission. */
    result = portal_request(c, "Start", g_variant_new("(osa{sv})", c->session, "", &options));
    if (!result) return -1;
    GVariant *streams = g_variant_lookup_value(result, "streams", G_VARIANT_TYPE("a(ua{sv})"));
    gboolean found = streams && g_variant_n_children(streams) > 0;
    if (found) {
        GVariant *entry = g_variant_get_child_value(streams, 0);
        GVariant *props;
        g_variant_get(entry, "(u@a{sv})", node, &props);
        guint64 object_serial;
        if (g_variant_lookup(props, "pipewire-serial", "t", &object_serial))
            *serial = g_strdup_printf("%" G_GUINT64_FORMAT, object_serial);
        g_variant_unref(props);
        g_variant_unref(entry);
    }
    if (streams) g_variant_unref(streams);
    g_variant_unref(result);
    if (!found) { fail(c, "nenhuma tela ou janela foi selecionada no portal"); return -1; }

    GUnixFDList *fds = NULL;
    g_variant_builder_init(&options, G_VARIANT_TYPE_VARDICT);
    result = g_dbus_connection_call_with_unix_fd_list_sync(c->bus,
        "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast", "OpenPipeWireRemote",
        g_variant_new("(oa{sv})", c->session, &options), G_VARIANT_TYPE("(h)"),
        G_DBUS_CALL_FLAGS_NONE, 5000, NULL, &fds, NULL, &error);
    if (!result) { dbus_fail(c, error); return -1; }
    gint index;
    g_variant_get(result, "(h)", &index);
    int fd = fds ? g_unix_fd_list_get(fds, index, &error) : -1;
    g_variant_unref(result);
    g_clear_object(&fds);
    if (fd < 0) dbus_fail(c, error);
    return fd;
}

static void on_core_error(void *user, uint32_t id, int seq, int res, const char *message) {
    (void)seq;
    if (id == PW_ID_CORE && res < 0) fail(user, message ? message : "conexão PipeWire interrompida");
}

static void on_state(void *user, enum pw_stream_state old, enum pw_stream_state state, const char *error) {
    (void)old;
    struct capture *c = user;
    if (state == PW_STREAM_STATE_ERROR) fail(c, error ? error : "erro no fluxo PipeWire");
    if (state == PW_STREAM_STATE_UNCONNECTED && c->ready_sent)
        fail(c, "o fluxo de captura PipeWire foi desconectado");
}

static void on_format(void *user, uint32_t id, const struct spa_pod *param) {
    struct capture *c = user;
    if (id != SPA_PARAM_Format || !param) return;
    uint32_t media_type, media_subtype;
    if (spa_format_parse(param, &media_type, &media_subtype) < 0 ||
        media_type != SPA_MEDIA_TYPE_video || media_subtype != SPA_MEDIA_SUBTYPE_raw ||
        spa_format_video_raw_parse(param, &c->format) < 0) {
        fail(c, "formato de vídeo PipeWire inválido"); return;
    }
    uint8_t storage[512];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *params[] = {
        spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
            SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int(
                (1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_MemFd))),
        spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta,
            SPA_PARAM_META_type, SPA_POD_Id(SPA_META_Header),
            SPA_PARAM_META_size, SPA_POD_Int(sizeof(struct spa_meta_header))),
        spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta,
            SPA_PARAM_META_type, SPA_POD_Id(SPA_META_VideoCrop),
            SPA_PARAM_META_size, SPA_POD_Int(sizeof(struct spa_meta_region)))
    };
    if (pw_stream_update_params(c->stream, params, SPA_N_ELEMENTS(params)) < 0)
        fail(c, "não foi possível negociar buffers de memória no PipeWire");
}

static void on_process(void *user) {
    struct capture *c = user;
    struct pw_buffer *buffer = NULL, *next;
    /* Always take the latest frame; promptly return all superseded buffers. */
    while ((next = pw_stream_dequeue_buffer(c->stream))) {
        if (buffer) pw_stream_queue_buffer(c->stream, buffer);
        buffer = next;
    }
    if (!buffer) return;
    struct spa_buffer *spa = buffer->buffer;
    if (!spa || spa->n_datas != 1) goto done;
    struct spa_data *plane = &spa->datas[0];
    if (plane->type != SPA_DATA_MemPtr && plane->type != SPA_DATA_MemFd) {
        fail(c, "o compositor não forneceu um buffer de memória CPU (MemFd/MemPtr)");
        goto done;
    }
    struct spa_meta_header *header = spa_buffer_find_meta_data(spa, SPA_META_Header, sizeof(*header));
    if (header && (header->flags & SPA_META_HEADER_FLAG_CORRUPTED)) goto done;
    if (!plane->chunk || !plane->chunk->size ||
        (plane->chunk->flags & SPA_CHUNK_FLAG_CORRUPTED)) goto done;
    if (!plane->data || plane->chunk->offset > plane->maxsize ||
        plane->chunk->size > plane->maxsize - plane->chunk->offset) {
        fail(c, "buffer PipeWire inacessível ou fora dos limites"); goto done;
    }
    uint32_t format;
    switch (c->format.format) {
    case SPA_VIDEO_FORMAT_BGRA: format = 0; break;
    case SPA_VIDEO_FORMAT_BGRx: format = 1; break;
    case SPA_VIDEO_FORMAT_RGBA: format = 2; break;
    case SPA_VIDEO_FORMAT_RGBx: format = 3; break;
    default: fail(c, "o compositor não forneceu um formato RGB suportado"); goto done;
    }
    uint32_t width = c->format.size.width, height = c->format.size.height;
    size_t skip = 0;
    struct spa_meta_region *crop = spa_buffer_find_meta_data(spa, SPA_META_VideoCrop, sizeof(*crop));
    if (crop && spa_meta_region_is_valid(crop)) {
        int32_t x = crop->region.position.x, y = crop->region.position.y;
        if (x < 0 || y < 0 || (uint32_t)x > width || (uint32_t)y > height ||
            crop->region.size.width > width - (uint32_t)x ||
            crop->region.size.height > height - (uint32_t)y || plane->chunk->stride <= 0) {
            fail(c, "região de captura inválida no frame PipeWire"); goto done;
        }
        uint64_t offset = (uint64_t)y * (uint32_t)plane->chunk->stride + (uint64_t)x * 4;
        if (offset >= plane->chunk->size) {
            fail(c, "região de captura fora do buffer PipeWire"); goto done;
        }
        skip = (size_t)offset;
        width = crop->region.size.width;
        height = crop->region.size.height;
    }
    if (c->frame(c->user, (const uint8_t *)plane->data + plane->chunk->offset + skip,
            plane->chunk->size - skip, width, height,
            plane->chunk->stride, format) != 0) {
        fail(c, "dimensões ou stride inválidos no frame PipeWire"); goto done;
    }
    if (!c->ready_sent) { c->ready_sent = TRUE; c->ready(c->user); }
done:
    pw_stream_queue_buffer(c->stream, buffer);
}

/* Runs synchronously on the Rust-owned capture worker. No callback outlives
 * this call. The remote fd is obtained from the portal, never the global PW
 * socket. Memory-only formats deliberately avoid GPU/DMA-BUF readback. */
void concord_wayland_run(void *user, stop_fn stopped, ready_fn ready,
        frame_fn frame, char *error, size_t error_len) {
    struct capture c = { .user = user, .stopped = stopped, .ready = ready,
        .frame = frame, .error = error, .error_len = error_len };
    c.glib_loop = g_main_context_new();
    g_main_context_push_thread_default(c.glib_loop);
    uint32_t node = PW_ID_ANY;
    char *serial = NULL;
    if (stopped(user)) goto cleanup;
    int fd = open_portal(&c, &node, &serial);
    if (fd < 0 || stopped(user)) { if (fd >= 0) close(fd); goto cleanup; }

    /* pw_init is process-wide and may be reached by concurrent captures. */
    static gsize initialized = 0;
    if (g_once_init_enter(&initialized)) { pw_init(NULL, NULL); g_once_init_leave(&initialized, 1); }
    c.loop = pw_main_loop_new(NULL);
    if (!c.loop) { close(fd); fail(&c, "não foi possível criar o loop PipeWire"); goto cleanup; }
    c.context = pw_context_new(pw_main_loop_get_loop(c.loop), NULL, 0);
    if (!c.context) { close(fd); fail(&c, "não foi possível criar o contexto PipeWire"); goto cleanup; }
    /* connect_fd takes ownership of fd, including on failure. */
    c.core = pw_context_connect_fd(c.context, fd, NULL, 0);
    if (!c.core) { fail(&c, "não foi possível conectar ao PipeWire autorizado pelo portal"); goto cleanup; }
    static const struct pw_core_events core_events = { .version = PW_VERSION_CORE_EVENTS, .error = on_core_error };
    pw_core_add_listener(c.core, &c.core_hook, &core_events, &c);
    struct pw_properties *props = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Video", PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Screen", NULL);
    if (serial) pw_properties_set(props, PW_KEY_TARGET_OBJECT, serial);
    c.stream = pw_stream_new(c.core, "Concord Screen", props);
    if (!c.stream) { fail(&c, "não foi possível criar o fluxo PipeWire"); goto cleanup; }
    static const struct pw_stream_events stream_events = {
        .version = PW_VERSION_STREAM_EVENTS, .state_changed = on_state,
        .param_changed = on_format, .process = on_process
    };
    pw_stream_add_listener(c.stream, &c.stream_hook, &stream_events, &c);
    uint8_t storage[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *params[] = {
        spa_pod_builder_add_object(&builder, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
            SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
            SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
            SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(5, SPA_VIDEO_FORMAT_BGRx,
                SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_RGBx, SPA_VIDEO_FORMAT_RGBA),
            SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(
                &SPA_RECTANGLE(1920, 1080), &SPA_RECTANGLE(1, 1), &SPA_RECTANGLE(8192, 8192)),
            SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(
                &SPA_FRACTION(60, 1), &SPA_FRACTION(0, 1), &SPA_FRACTION(60, 1)))
    };
    int result = pw_stream_connect(c.stream, PW_DIRECTION_INPUT, serial ? PW_ID_ANY : node,
        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS, params, 1);
    if (result < 0) { fail(&c, "não foi possível iniciar o fluxo PipeWire"); goto cleanup; }
    gint64 deadline = g_get_monotonic_time() + 15 * G_TIME_SPAN_SECOND;
    pw_loop_enter(pw_main_loop_get_loop(c.loop));
    while (!stopped(user) && !c.error[0]) {
        pump_portal(&c);
        result = pw_loop_iterate(pw_main_loop_get_loop(c.loop), 50);
        if (result < 0 && result != -EINTR) fail(&c, "o loop PipeWire falhou");
        if (!c.ready_sent && g_get_monotonic_time() > deadline)
            fail(&c, "o PipeWire não entregou o primeiro frame em 15 segundos");
    }
    pw_loop_leave(pw_main_loop_get_loop(c.loop));
cleanup:
    if (c.stream) pw_stream_destroy(c.stream);
    if (c.core) pw_core_disconnect(c.core);
    if (c.context) pw_context_destroy(c.context);
    if (c.loop) pw_main_loop_destroy(c.loop);
    close_object(&c, c.session, "org.freedesktop.portal.Session");
    if (c.response_sub) g_dbus_connection_signal_unsubscribe(c.bus, c.response_sub);
    if (c.closed_sub) g_dbus_connection_signal_unsubscribe(c.bus, c.closed_sub);
    if (c.bus) { g_dbus_connection_close_sync(c.bus, NULL, NULL); g_object_unref(c.bus); }
    if (c.response) g_variant_unref(c.response);
    g_free(c.session);
    g_free(c.request);
    g_free(serial);
    /* Release queued signal callbacks before the stack-allocated user data. */
    for (int i = 0; i < 64 && g_main_context_pending(c.glib_loop); i++)
        g_main_context_iteration(c.glib_loop, FALSE);
    g_main_context_pop_thread_default(c.glib_loop);
    g_main_context_unref(c.glib_loop);
}
