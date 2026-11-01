import assert from 'node:assert/strict';
import { test } from 'node:test';
import { linkify } from '../src/lib/linkify.ts';
import { captureShortcut, matchesShortcut } from '../src/lib/shortcut.ts';
import { readKey, readPosition, unreadMessages } from '../src/lib/unread.ts';

test('links preserve surrounding text and activate only HTTP or HTTPS', () => {
  const text = 'Veja (https://example.com/foto?q=1), javascript:alert(1) e https://outro.test/x.';
  const parts = linkify(text);
  assert.equal(parts.map(part => part.text).join(''), text);
  assert.deepEqual(parts.filter(part => part.url).map(part => part.url), ['https://example.com/foto?q=1', 'https://outro.test/x']);
  assert.deepEqual(linkify('<script>alert(1)</script> file:///etc/passwd'), [{text:'<script>alert(1)</script> file:///etc/passwd'}]);
});

test('mute shortcuts match exact modifiers and do not repeat or interrupt composition', () => {
  const event = { key:'m', ctrlKey:true, shiftKey:true, altKey:false, metaKey:false, repeat:false, isComposing:false };
  assert.equal(captureShortcut(event), 'Ctrl+Shift+M');
  assert.equal(matchesShortcut(event, 'Ctrl+Shift+M'), true);
  assert.equal(matchesShortcut({...event, altKey:true}, 'Ctrl+Shift+M'), false);
  assert.equal(matchesShortcut({...event, repeat:true}, 'Ctrl+Shift+M'), false);
  assert.equal(matchesShortcut({...event, isComposing:true}, 'Ctrl+Shift+M'), false);
  assert.equal(captureShortcut({...event, ctrlKey:false}), null);
  assert.equal(matchesShortcut(event, ''), false);
});

test('read positions isolate servers and rooms; unread counts exclude own stable identity', () => {
  assert.notEqual(readKey('https://a.test','geral'), readKey('https://b.test','geral'));
  assert.notEqual(readKey('https://a.test','geral'), readKey('https://a.test','outra'));
  assert.equal(readPosition({getItem:()=>null}, 'key'), null);
  assert.equal(readPosition({getItem:()=>'-1'}, 'key'), null);
  assert.equal(readPosition({getItem:()=>'NaN'}, 'key'), null);
  assert.equal(readPosition({getItem:()=>'41'}, 'key'), 41);
  const messages = [{id:1,user_id:'other'}, {id:2,user_id:'old-session',author_id:'my-author'}, {id:3,user_id:'me'}, {id:4,user_id:'other',author_id:'other-author'}];
  assert.deepEqual(unreadMessages(messages,1,'me','my-author').map(item=>item.id), [4]);
});
