const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const script = fs.readFileSync(path.join(__dirname, 'watch_party_local.js'), 'utf8');
const media = { content_type: 'series', metadata_id: 'tt1234567', video_id: 'tt1234567:1:2' };
const otherMedia = { ...media, video_id: 'tt1234567:1:3' };
const privateUrl = 'https://debrid.example/host-private-token/video.mkv';
const addonUrl = 'https://addon.example/host-private-api-key/manifest.json';

const flush = () => new Promise(resolve => setImmediate(resolve));

function selected(identity = media, url = privateUrl) {
    return {
        stream: { url, deepLinks: { player: '/player/encoded-private-url' } },
        metaRequest: { base: addonUrl, path: { resource: 'meta', type: identity.content_type, id: identity.metadata_id, extra: [] } },
        streamRequest: { base: addonUrl, path: { resource: 'stream', type: identity.content_type, id: identity.video_id, extra: [] } },
        subtitlesPath: { resource: 'subtitles', type: identity.content_type, id: identity.video_id, extra: [] }
    };
}

function load(identity = media, url = privateUrl) {
    return { action: 'Load', args: { model: 'Player', args: selected(identity, url) } };
}

function deferred() {
    let resolve, reject;
    const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
    return { promise, resolve, reject };
}

async function bridge({ lateCore = false, getState = async () => ({ selected: null }) } = {}) {
    const posts = [], order = [], timers = [], handlers = [];
    const core = {
        getState,
        dispatch(action, model) { order.push('dispatch'); return Promise.resolve({ action, model }); }
    };
    const window = {
        location: { hash: '#/player/private-encoded-host-route' },
        chrome: { webview: {
            postMessage(raw) { order.push('post'); posts.push(JSON.parse(raw)); },
            addEventListener(name, handler) { if (name === 'message') handlers.push(handler); }
        } }
    };
    window.self = window.top = window;
    if (!lateCore) window.core = core;
    vm.runInNewContext(script, {
        window,
        setTimeout(callback) { timers.push(callback); return timers.length; }
    });
    await flush();
    posts.length = 0;
    order.length = 0;
    return {
        window, core, posts, order, timers,
        emit(args) { for (const handler of handlers) handler({ data: { args } }); },
        mediaPosts() { return posts.filter(post => post.args[0] === 'shell-watch-party-media').map(post => post.args[1]); }
    };
}

function assertNoPrivateSource(posts) {
    const encoded = JSON.stringify(posts);
    for (const secret of [privateUrl, addonUrl, 'host-private-token', 'host-private-api-key', 'encoded-private-url']) {
        assert.equal(encoded.includes(secret), false, `bridge leaked ${secret}`);
    }
}

test('captures current Web Player Load before dispatch and posts only semantic identity', async () => {
    const view = await bridge();
    const result = await view.core.dispatch(load(), 'player');
    assert.equal(result.model, 'player');
    assert.deepEqual(view.order, ['post', 'dispatch']);
    assert.deepEqual(view.mediaPosts()[0].media, media);
    assert.equal(view.mediaPosts()[0].load_intent, true);
    assert.equal(view.mediaPosts()[0].request_id, null);
    assertNoPrivateSource(view.posts);
});

test('wraps late window.core assignment before React can dispatch the first load', async () => {
    const view = await bridge({ lateCore: true });
    view.window.core = view.core;
    view.core.dispatch(load(), 'player');
    await flush();
    assert.deepEqual(view.order, ['post', 'dispatch']);
    assert.equal(view.mediaPosts().length, 1);
    assert.equal(view.mediaPosts()[0].load_intent, true);
});

test('opens the guest stream selector using IDs and binds the subsequent local selection', async () => {
    const view = await bridge();
    view.emit(['shell-watch-party-select-source', { media, generation: 7, request_id: 'request-seven',
        web_url: 'https://debrid.example/never-open-this' }]);
    assert.equal(view.window.location.hash, '#/metadetails/series/tt1234567/tt1234567%3A1%3A2');
    view.core.dispatch(load(media, 'https://guest-debrid.example/guest-token'), 'player');
    const captured = view.mediaPosts().at(-1);
    assert.equal(captured.generation, 7);
    assert.equal(captured.request_id, 'request-seven');
    assert.equal(captured.load_intent, true);
    assert.equal(JSON.stringify(view.posts).includes('guest-token'), false);
    assertNoPrivateSource(view.posts);
});

test('wrong episodes and unloading never inherit the pending guest request', async () => {
    const view = await bridge();
    view.emit(['shell-watch-party-select-source', { media, generation: 8, request_id: 'request-eight' }]);
    view.core.dispatch(load(otherMedia), 'player');
    assert.equal(view.mediaPosts().at(-1).generation, null);
    assert.equal(view.mediaPosts().at(-1).request_id, null);
    view.core.dispatch({ action: 'Unload' }, 'player');
    assert.equal(view.mediaPosts().at(-1).media, null);
    assert.equal(view.mediaPosts().at(-1).load_intent, true);
    view.core.dispatch(load(media), 'player');
    assert.equal(view.mediaPosts().at(-1).request_id, 'request-eight');
});

test('current-state snapshots preserve selection identity and do not become load intents', async () => {
    const view = await bridge({ getState: async () => ({ selected: selected() }) });
    view.core.dispatch(load(), 'player');
    const selectionId = view.mediaPosts().at(-1).selection_id;
    view.emit(['shell-watch-party-media-get']);
    await flush();
    const snapshot = view.mediaPosts().at(-1);
    assert.equal(snapshot.selection_id, selectionId);
    assert.equal(snapshot.load_intent, false);
    assertNoPrivateSource(view.posts);
});

test('an already playing snapshot cannot attach a fresh guest-selection ticket', async () => {
    const view = await bridge({ getState: async () => ({ selected: selected() }) });
    view.emit(['shell-watch-party-select-source', { media, generation: 9, request_id: 'request-nine' }]);
    view.emit(['shell-watch-party-media-get']);
    await flush();
    const snapshot = view.mediaPosts().at(-1);
    assert.equal(snapshot.request_id, null);
    assert.equal(snapshot.generation, null);
    assert.equal(snapshot.load_intent, false);
});

test('lagging same-episode core state cannot erase a newly selected guest source ticket', async () => {
    let current = selected();
    const guestUrl = 'https://guest-debrid.example/new-guest-source';
    const view = await bridge({ getState: async () => ({ selected: current }) });
    view.emit(['shell-watch-party-select-source', { media, generation: 9, request_id: 'request-nine' }]);
    view.core.dispatch(load(media, guestUrl), 'player');
    const captured = view.mediaPosts().at(-1);
    view.emit(['shell-watch-party-media-get']);
    await flush();
    assert.equal(view.mediaPosts().length, 1);
    current = selected(media, guestUrl);
    view.emit(['shell-watch-party-media-get']);
    await flush();
    const snapshot = view.mediaPosts().at(-1);
    assert.equal(snapshot.selection_id, captured.selection_id);
    assert.equal(snapshot.request_id, 'request-nine');
    assert.equal(snapshot.generation, 9);
    assert.equal(snapshot.load_intent, false);
    assertNoPrivateSource(view.posts);
});

test('lagging core state cannot restore an unloaded player binding', async () => {
    let current = selected();
    const view = await bridge({ getState: async () => ({ selected: current }) });
    view.core.dispatch(load(), 'player');
    view.core.dispatch({ action: 'Unload' }, 'player');
    view.emit(['shell-watch-party-media-get']);
    await flush();
    assert.equal(view.mediaPosts().length, 2);
    assert.equal(view.mediaPosts().at(-1).media, null);
    current = null;
    view.emit(['shell-watch-party-media-get']);
    await flush();
    assert.equal(view.mediaPosts().at(-1).media, null);
    assert.equal(view.mediaPosts().at(-1).load_intent, false);
});

test('late getState replies cannot overwrite a new episode request or load dispatch', async () => {
    const waits = [];
    const view = await bridge({ getState: () => { const waiting = deferred(); waits.push(waiting); return waiting.promise; } });
    view.emit(['shell-watch-party-media-get']);
    view.emit(['shell-watch-party-select-source', { media: otherMedia, generation: 10, request_id: 'request-ten' }]);
    view.core.dispatch(load(otherMedia), 'player');
    for (const waiting of waits) waiting.resolve({ selected: selected() });
    await flush();
    assert.equal(view.mediaPosts().length, 1);
    assert.deepEqual(view.mediaPosts()[0].media, otherMedia);
    assert.equal(view.mediaPosts()[0].request_id, 'request-ten');
});

test('stale requests, duplicate requests and invalid IDs cannot replace the stream selector', async () => {
    const view = await bridge();
    view.emit(['shell-watch-party-select-source', { media: otherMedia, generation: 11, request_id: 'request-eleven' }]);
    const route = view.window.location.hash;
    view.emit(['shell-watch-party-select-source', { media, generation: 10, request_id: 'request-ten' }]);
    view.emit(['shell-watch-party-select-source', { media: { ...media, metadata_id: privateUrl }, generation: 12, request_id: 'bad' }]);
    assert.equal(view.window.location.hash, route);
    view.window.location.hash = '#/player/guest-stream';
    view.emit(['shell-watch-party-select-source', { media: otherMedia, generation: 11, request_id: 'request-eleven' }]);
    assert.equal(view.window.location.hash, '#/player/guest-stream');
});

test('leaving clears pending tickets and unrelated model unloads do not clear media', async () => {
    const view = await bridge();
    view.emit(['shell-watch-party-select-source', { media, generation: 12, request_id: 'request-twelve' }]);
    view.core.dispatch({ action: 'Unload' }, 'meta_details');
    assert.equal(view.mediaPosts().length, 0);
    view.emit(['shell-watch-party-local-clear']);
    view.core.dispatch(load(), 'player');
    assert.equal(view.mediaPosts().at(-1).request_id, null);
});

test('leaving removes a selected guest ticket from later current-state snapshots', async () => {
    const view = await bridge({ getState: async () => ({ selected: selected() }) });
    view.emit(['shell-watch-party-select-source', { media, generation: 12, request_id: 'request-twelve' }]);
    view.core.dispatch(load(), 'player');
    const selectionId = view.mediaPosts().at(-1).selection_id;
    view.emit(['shell-watch-party-local-clear']);
    view.emit(['shell-watch-party-media-get']);
    await flush();
    const snapshot = view.mediaPosts().at(-1);
    assert.equal(snapshot.selection_id, selectionId);
    assert.equal(snapshot.generation, null);
    assert.equal(snapshot.request_id, null);
    assert.equal(snapshot.load_intent, false);
});

test('a missing core is bounded and surfaces an actionable guest selection error', async () => {
    const view = await bridge({ lateCore: true });
    view.emit(['shell-watch-party-select-source', { media, generation: 13, request_id: 'request-thirteen' }]);
    for (let index = 0; index < view.timers.length; index++) view.timers[index]();
    assert.equal(view.timers.length, 119);
    const errors = view.posts.filter(post => post.args[0] === 'shell-watch-party-local-error');
    assert.equal(errors.length, 1);
    assert.match(errors[0].args[1].message, /Update Stremio Web/);
});
