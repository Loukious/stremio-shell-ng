const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

async function player(language = 'eng') {
    const elements = new Map(), events = {}, timers = new Map();
    let receive, now = 0, timerId = 0;
    const location = { hash: '#/player/one' };
    const window = {
        chrome: { webview: { addEventListener: (_, listener) => { receive = listener; }, postMessage() {} } },
        core: { getState: async () => ({ profile: { settings: { interfaceLanguage: language } } }) },
        addEventListener: (name, listener) => { events[name] = listener; }
    };
    window.self = window.top = window;
    const document = {
        body: { appendChild: element => elements.set(element.id, element) }, documentElement: {},
        createElement: () => ({ setAttribute() {}, hidden: false }),
        getElementById: id => elements.get(id), querySelectorAll: () => [], addEventListener() {}
    };
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'intro_skip.js'), 'utf8'), {
        window, document, location,
        MutationObserver: class { observe() {} }, setInterval() {},
        setTimeout: (callback, delay) => { const id = ++timerId; timers.set(id, { callback, at: now + delay }); return id; },
        clearTimeout: id => timers.delete(id)
    });
    await new Promise(setImmediate);
    return {
        notify: kind => receive({ data: { args: ['shell-auto-skip', { kind }] } }),
        toast: () => elements.get('stremio-shell-auto-skip-toast'),
        advance: milliseconds => {
            now += milliseconds;
            for (const [id, timer] of timers) {
                if (timer.at <= now) { timers.delete(id); timer.callback(); }
            }
        },
        navigate: hash => { location.hash = hash; events.hashchange(); }
    };
}

test('announces openings and endings in the interface language for three seconds', async () => {
    for (const [language, intro, outro] of [
        ['eng', 'Opening skipped', 'Ending skipped'],
        ['es-ES', 'Opening saltado', 'Ending saltado'],
        ['spa', 'Opening saltado', 'Ending saltado']
    ]) {
        const p = await player(language);
        p.notify('recap');
        assert.equal(p.toast(), undefined);
        p.notify('intro');
        assert.equal(p.toast().textContent, intro);
        p.advance(2999);
        assert.equal(p.toast().hidden, false);
        p.advance(1);
        assert.equal(p.toast().hidden, true);
        p.notify('outro');
        assert.equal(p.toast().textContent, outro);
        assert.equal(p.toast().hidden, false);
    }
});

test('a new skip replaces the notice and restarts its timeout', async () => {
    const p = await player();
    p.notify('intro');
    p.advance(1000);
    p.notify('outro');
    p.advance(2000);
    assert.equal(p.toast().textContent, 'Ending skipped');
    assert.equal(p.toast().hidden, false);
    p.advance(1000);
    assert.equal(p.toast().hidden, true);
});

test('keeps the ending notice across episodes and clears it when leaving the player', async () => {
    const p = await player();
    p.notify('outro');
    p.navigate('#/player/two');
    assert.equal(p.toast().hidden, false);
    p.navigate('#/library');
    assert.equal(p.toast().hidden, true);
});
