-- Concord owns only these prefixed tables. No application or provider tables
-- outside this namespace are inspected or deleted.
CREATE TABLE IF NOT EXISTS concord_chat_messages (
    id BIGSERIAL PRIMARY KEY,
    room_id TEXT NOT NULL,
    request_id UUID NOT NULL,
    content_hash TEXT NOT NULL,
    payload JSONB NOT NULL,
    stored_bytes BIGINT NOT NULL CHECK (stored_bytes > 0),
    object_key TEXT,
    image_bytes BIGINT NOT NULL DEFAULT 0 CHECK (image_bytes >= 0),
    UNIQUE (room_id, request_id)
);
-- concord:statement
CREATE INDEX IF NOT EXISTS concord_chat_room_history ON concord_chat_messages (room_id, id DESC);
-- concord:statement
CREATE TABLE IF NOT EXISTS concord_chat_garbage (
    object_key TEXT PRIMARY KEY,
    image_bytes BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- concord:statement
CREATE TABLE IF NOT EXISTS concord_chat_staged (
    object_key TEXT PRIMARY KEY,
    image_bytes BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- concord:statement
ALTER TABLE concord_chat_garbage ADD COLUMN IF NOT EXISTS deleted_parts BIGINT NOT NULL DEFAULT 0;
-- concord:statement
ALTER TABLE concord_chat_messages ADD COLUMN IF NOT EXISTS owner_id TEXT;
-- concord:statement
CREATE OR REPLACE FUNCTION concord_chat_payload(p_payload JSONB) RETURNS JSONB LANGUAGE sql STABLE AS $$
 SELECT p_payload || jsonb_build_object('reply', (
   SELECT jsonb_build_object('id',id,'display_name',payload->>'display_name',
     'text',left(coalesce(nullif(payload->>'text',''),payload->'attachment'->>'name','Imagem'),180))
   FROM concord_chat_messages WHERE id=(p_payload->>'reply_to')::bigint AND room_id=p_payload->>'room_id'
 ));
$$;
-- concord:statement
CREATE OR REPLACE FUNCTION concord_chat_append(
    p_room TEXT, p_request UUID, p_hash TEXT, p_payload JSONB,
    p_key TEXT, p_image_bytes BIGINT, p_max_bytes BIGINT,
    p_max_images BIGINT, p_max_count BIGINT
) RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE
    previous concord_chat_messages%ROWTYPE;
    victim concord_chat_messages%ROWTYPE;
    total_bytes BIGINT;
    total_images BIGINT;
    total_count BIGINT;
    new_bytes BIGINT := octet_length(p_payload::text) + 4096;
    new_id BIGINT;
    saved JSONB;
    removed JSONB := '[]'::jsonb;
BEGIN
    PERFORM pg_advisory_xact_lock(764310826);
    SELECT * INTO previous FROM concord_chat_messages
        WHERE room_id = p_room AND request_id = p_request;
    IF FOUND THEN
        IF previous.content_hash <> p_hash THEN
            RAISE EXCEPTION 'concord_request_conflict' USING ERRCODE = '22000';
        END IF;
        RETURN jsonb_build_object('message', concord_chat_payload(previous.payload), 'deleted', removed, 'created', false);
    END IF;
    IF new_bytes > p_max_bytes OR p_image_bytes > p_max_images THEN
        RAISE EXCEPTION 'concord_item_too_large' USING ERRCODE = '22023';
    END IF;
    SELECT coalesce(sum(stored_bytes), 0), coalesce(sum(image_bytes), 0), count(*)
        INTO total_bytes, total_images, total_count FROM concord_chat_messages;
    total_images := total_images
        + (SELECT coalesce(sum(image_bytes), 0) FROM concord_chat_garbage)
        + (SELECT coalesce(sum(image_bytes), 0) FROM concord_chat_staged WHERE object_key IS DISTINCT FROM p_key);
    IF total_bytes + new_bytes > p_max_bytes OR total_images + p_image_bytes > p_max_images OR total_count + 1 > p_max_count THEN
        IF EXISTS(SELECT 1 FROM concord_chat_garbage) THEN
            RAISE EXCEPTION 'concord_cleanup_pending' USING ERRCODE = 'P0001';
        END IF;
        FOR victim IN SELECT * FROM concord_chat_messages ORDER BY id LOOP
            EXIT WHEN total_bytes + new_bytes <= p_max_bytes
                AND total_images + p_image_bytes <= p_max_images AND total_count + 1 <= p_max_count;
            IF victim.object_key IS NOT NULL THEN
                INSERT INTO concord_chat_garbage(object_key, image_bytes)
                    VALUES (victim.object_key, victim.image_bytes) ON CONFLICT DO NOTHING;
            END IF;
            DELETE FROM concord_chat_messages WHERE id = victim.id;
            total_bytes := total_bytes - victim.stored_bytes;
            total_images := total_images - victim.image_bytes;
            total_count := total_count - 1;
            removed := removed || jsonb_build_array(jsonb_build_object('room_id', victim.room_id, 'id', victim.id));
        END LOOP;
    END IF;
    IF total_bytes + new_bytes > p_max_bytes OR total_images + p_image_bytes > p_max_images THEN
        -- Roll back the entire eviction if unrelated/staged objects prevent admission.
        RAISE EXCEPTION 'concord_cleanup_pending' USING ERRCODE = 'P0001';
    END IF;
    INSERT INTO concord_chat_messages(room_id, request_id, content_hash, payload, stored_bytes, object_key, image_bytes, owner_id)
        VALUES (p_room, p_request, p_hash, p_payload, new_bytes, p_key, p_image_bytes, p_payload->>'author_id') RETURNING id INTO new_id;
    saved := (p_payload - 'reply') || jsonb_build_object('id', new_id);
    UPDATE concord_chat_messages SET payload = saved WHERE id = new_id;
    DELETE FROM concord_chat_staged WHERE object_key = p_key;
    RETURN jsonb_build_object('message', concord_chat_payload(saved), 'deleted', removed, 'created', true);
END;
$$;
-- concord:statement
CREATE OR REPLACE FUNCTION concord_chat_evict_one(p_images_only BOOLEAN)
RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE victim concord_chat_messages%ROWTYPE;
BEGIN
    PERFORM pg_advisory_xact_lock(764310826);
    IF EXISTS(SELECT 1 FROM concord_chat_garbage) THEN
        RAISE EXCEPTION 'concord_cleanup_pending' USING ERRCODE = 'P0001';
    END IF;
    SELECT * INTO victim FROM concord_chat_messages
        WHERE NOT p_images_only OR object_key IS NOT NULL ORDER BY id LIMIT 1;
    IF NOT FOUND THEN RETURN '[]'::jsonb; END IF;
    IF victim.object_key IS NOT NULL THEN
        INSERT INTO concord_chat_garbage(object_key, image_bytes)
            VALUES (victim.object_key, victim.image_bytes) ON CONFLICT DO NOTHING;
    END IF;
    DELETE FROM concord_chat_messages WHERE id = victim.id;
    RETURN jsonb_build_array(jsonb_build_object('room_id', victim.room_id, 'id', victim.id));
END;
$$;

-- concord:statement
CREATE OR REPLACE FUNCTION concord_chat_edit(p_room TEXT,p_id BIGINT,p_user TEXT,p_author TEXT,p_text TEXT,p_limit BIGINT)
RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE victim concord_chat_messages%ROWTYPE; saved JSONB; cost BIGINT;
BEGIN
 PERFORM pg_advisory_xact_lock(764310826);
 SELECT * INTO victim FROM concord_chat_messages WHERE room_id=p_room AND id=p_id;
 IF NOT FOUND THEN RAISE EXCEPTION 'concord_message_missing' USING ERRCODE='P0002'; END IF;
 IF NOT ((victim.owner_id IS NOT NULL AND victim.owner_id IS NOT DISTINCT FROM p_author)
     OR (victim.owner_id IS NULL AND victim.payload->>'user_id' IS NOT DISTINCT FROM p_user)) THEN
   RAISE EXCEPTION 'concord_message_forbidden' USING ERRCODE='42501';
 END IF;
 IF p_text='' AND victim.object_key IS NULL THEN RAISE EXCEPTION 'concord_empty_message' USING ERRCODE='22023'; END IF;
 saved := victim.payload || jsonb_build_object('text',p_text,'author_id',coalesce(victim.owner_id,p_author),
   'edited_at_ms',greatest((extract(epoch from clock_timestamp())*1000)::bigint,coalesce((victim.payload->>'edited_at_ms')::bigint,0)+1));
 cost := octet_length(saved::text)+4096;
 IF (SELECT coalesce(sum(stored_bytes),0) FROM concord_chat_messages)-victim.stored_bytes+cost>p_limit THEN
   RAISE EXCEPTION 'concord_edit_too_large' USING ERRCODE='22023';
 END IF;
 UPDATE concord_chat_messages SET payload=saved,stored_bytes=cost,owner_id=coalesce(owner_id,p_author) WHERE id=p_id;
 RETURN concord_chat_payload(saved);
END;
$$;
-- concord:statement
CREATE OR REPLACE FUNCTION concord_chat_delete(p_room TEXT,p_id BIGINT,p_user TEXT,p_author TEXT)
RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE victim concord_chat_messages%ROWTYPE;
BEGIN
 PERFORM pg_advisory_xact_lock(764310826);
 SELECT * INTO victim FROM concord_chat_messages WHERE room_id=p_room AND id=p_id;
 IF NOT FOUND THEN RETURN '[]'::jsonb; END IF;
 IF NOT ((victim.owner_id IS NOT NULL AND victim.owner_id IS NOT DISTINCT FROM p_author)
     OR (victim.owner_id IS NULL AND victim.payload->>'user_id' IS NOT DISTINCT FROM p_user)) THEN
   RAISE EXCEPTION 'concord_message_forbidden' USING ERRCODE='42501';
 END IF;
 IF victim.object_key IS NOT NULL THEN
   INSERT INTO concord_chat_garbage(object_key,image_bytes) VALUES(victim.object_key,victim.image_bytes) ON CONFLICT DO NOTHING;
 END IF;
 DELETE FROM concord_chat_messages WHERE id=p_id;
 RETURN jsonb_build_array(jsonb_build_object('room_id',p_room,'id',p_id));
END;
$$;

-- concord:statement
CREATE TABLE IF NOT EXISTS concord_accounts (
 id TEXT PRIMARY KEY,
 username TEXT UNIQUE NOT NULL,
 display_name TEXT NOT NULL,
 password_hash TEXT NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- concord:statement
CREATE TABLE IF NOT EXISTS concord_account_sessions (
 token_hash TEXT PRIMARY KEY,
 account_id TEXT NOT NULL REFERENCES concord_accounts(id) ON DELETE CASCADE,
 expires_at_ms BIGINT NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- concord:statement
CREATE INDEX IF NOT EXISTS concord_account_session_owner ON concord_account_sessions(account_id,created_at);
-- concord:statement
CREATE OR REPLACE FUNCTION concord_account_create(p_id TEXT,p_username TEXT,p_display TEXT,p_hash TEXT)
RETURNS JSONB LANGUAGE plpgsql AS $$
BEGIN
 PERFORM pg_advisory_xact_lock(764310827);
 IF EXISTS(SELECT 1 FROM concord_accounts WHERE username=p_username) THEN RETURN jsonb_build_object('created',false); END IF;
 IF (SELECT count(*) FROM concord_accounts)>=1000 THEN RETURN jsonb_build_object('full',true); END IF;
 INSERT INTO concord_accounts(id,username,display_name,password_hash) VALUES(p_id,p_username,p_display,p_hash);
 RETURN jsonb_build_object('created',true);
END;
$$;
-- concord:statement
CREATE OR REPLACE FUNCTION concord_account_session_create(p_id TEXT,p_hash TEXT,p_expires BIGINT)
RETURNS JSONB LANGUAGE plpgsql AS $$
BEGIN
 PERFORM pg_advisory_xact_lock(764310827);
 DELETE FROM concord_account_sessions WHERE expires_at_ms <= (extract(epoch FROM clock_timestamp())*1000)::bigint;
 DELETE FROM concord_account_sessions WHERE token_hash IN
   (SELECT token_hash FROM concord_account_sessions WHERE account_id=p_id ORDER BY created_at DESC,token_hash DESC OFFSET 7);
 INSERT INTO concord_account_sessions(token_hash,account_id,expires_at_ms) VALUES(p_hash,p_id,p_expires);
 RETURN 'true'::jsonb;
END;
$$;
