const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const FULLSCREEN_PATHS = [
    'M406.5 311.9v95.8h-95.8M105.5 202.5v-95.8h95.8M310.7 106.7h95.8v95.8M201.3 407.7h-95.8v-95.8',
    'M310.7 407.7v-95.8h95.8M201.3 106.7v95.8h-95.8M406.5 202.5h-95.8v-95.8M105.5 311.99h95.8v95.8'
];

async function player(language = 'eng', { bodyReady = true } = {}) {
    const elements = [], events = {}, documentEvents = {}, timers = new Map(), posts = [];
    let receive, now = 0, timerId = 0;
    const location = { hash: '#/player/one' };

    // The isolated tests exercise message and mount timing; browser checks cover
    // native radio activation and the rendered Stremio fullscreen SVGs.
    class Element {
        constructor(tag) {
            this.tagName = tag.toUpperCase();
            this.attributes = new Map();
            this.children = [];
            this.parts = new Map();
            this.listeners = {};
            this.style = {};
            this.className = '';
            this.hidden = false;
            this.rect = { width: 44, height: 44, right: 100, bottom: 44 };
            this.classList = { add() {}, remove() {} };
            elements.push(this);
        }
        get isConnected() { return this.root || !!this.parentElement?.isConnected; }
        get nextElementSibling() {
            return this.parentElement?.children[this.parentElement.children.indexOf(this) + 1];
        }
        setAttribute(name, value) { this.attributes.set(name, value); }
        getAttribute(name) { return this.attributes.get(name) || null; }
        getBoundingClientRect() { return this.rect; }
        appendChild(element) { return this.insertBefore(element, null); }
        insertBefore(element, before) {
            element.remove();
            const index = before ? this.children.indexOf(before) : this.children.length;
            this.children.splice(index, 0, element);
            element.parentElement = this;
            return element;
        }
        remove() {
            if (this.parentElement) this.parentElement.children.splice(this.parentElement.children.indexOf(this), 1);
            this.parentElement = null;
        }
        addEventListener(name, listener) { (this.listeners[name] ||= []).push(listener); }
        click(detail = 1) { for (const listener of this.listeners.click || []) listener({ target: this, detail, stopPropagation() {} }); }
        focus() { document.activeElement = this; }
        contains(element) { return element === this || this.children.some(child => child.contains(element)); }
        closest(selector) {
            for (let element = this; element; element = element.parentElement) {
                if (selector.startsWith('#') && element.id === selector.slice(1)) return element;
                if (selector.startsWith('.') && element.className.split(/\s+/).includes(selector.slice(1))) return element;
                if (element.tagName.toLowerCase() === selector) return element;
            }
            return null;
        }
        set innerHTML(html) {
            if (this.tagName === 'FIELDSET') {
                for (const value of ['auto', 'manual']) {
                    const label = this.appendChild(new Element('label'));
                    const input = label.appendChild(new Element('input'));
                    input.value = value;
                    input.name = 'shell-intro-mode';
                }
            }
        }
        querySelector(selector) {
            if (selector === 'input:checked') return this.querySelectorAll('input').find(input => input.checked);
            if (!this.parts.has(selector)) this.parts.set(selector, new Element('span'));
            return this.parts.get(selector);
        }
        querySelectorAll(selector) {
            if (selector === 'svg path') return this.paths || [];
            if (selector === 'input') return this.children.flatMap(label => label.children);
            return [];
        }
    }

    const window = {
        chrome: { webview: {
            addEventListener: (_, listener) => { receive = listener; },
            postMessage: message => posts.push(JSON.parse(message))
        } },
        core: { getState: async () => ({ profile: { settings: { interfaceLanguage: language } } }) },
        addEventListener: (name, listener) => { events[name] = listener; }
    };
    window.self = window.top = window;
    const document = {
        documentElement: {},
        createElement: tag => new Element(tag),
        getElementById: id => elements.find(element => element.id === id && element.isConnected),
        querySelectorAll: () => elements.filter(element => element.isConnected && (element.getAttribute('title') || element.getAttribute('aria-label'))),
        addEventListener: (name, listener) => { documentEvents[name] = listener; }
    };
    function mountBody() {
        document.body = new Element('body');
        document.head = new Element('head');
        document.body.root = document.head.root = true;
    }
    if (bodyReady) mountBody();
    const context = vm.createContext({
        window, document, location,
        MutationObserver: class { observe() {} }, setInterval() {},
        getComputedStyle: () => ({ transitionDuration: '0' }), requestAnimationFrame: callback => callback(),
        setTimeout: (callback, delay) => { const id = ++timerId; timers.set(id, { callback, at: now + delay }); return id; },
        clearTimeout: id => timers.delete(id)
    });
    for (const script of ['player_controls.js', 'intro_skip.js']) {
        vm.runInContext(fs.readFileSync(path.join(__dirname, script), 'utf8'), context);
    }
    await new Promise(setImmediate);
    const message = (method, params) => receive({ data: { args: [method, params] } });
    return {
        notify: kind => message('shell-auto-skip', { kind }),
        offer: (offerId, visible = true) => message('shell-manual-skip-state', { visible, kind: 'intro', offer_id: offerId }),
        toast: () => document.getElementById('stremio-shell-auto-skip-toast'),
        skipButton: () => document.getElementById('stremio-shell-manual-skip-btn'),
        settingsButton: () => document.getElementById('stremio-shell-intro-skip-settings-btn'),
        activeElement: () => document.activeElement,
        pointerDown: element => {
            let prevented = false;
            for (const listener of element.listeners.mousedown || []) listener({ preventDefault() { prevented = true; } });
            if (!prevented) element.focus();
        },
        keydown: key => {
            const event = {
                key, target: document.activeElement || document.body, stopped: false, defaultPrevented: false,
                preventDefault() { this.defaultPrevented = true; },
                stopImmediatePropagation() { this.stopped = true; }
            };
            documentEvents.keydown(event);
            return event;
        },
        hasStyles: () => document.head?.children.some(element => element.textContent?.includes('#stremio-shell-manual-skip-btn')),
        ready: () => { mountBody(); documentEvents.DOMContentLoaded(); },
        posts,
        fullscreen: (title, pathData, visible = true) => {
            const element = document.body.appendChild(new Element('div'));
            element.setAttribute('title', title);
            const svgPath = new Element('path');
            svgPath.setAttribute('d', pathData);
            element.paths = [svgPath];
            if (!visible) element.rect = { width: 0, height: 0 };
            return element;
        },
        findFullscreen: () => window.__stremioShellFindFullscreenControl(),
        refresh: () => documentEvents.DOMContentLoaded(),
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

test('finds translated enter/exit fullscreen icons and ignores hidden controls', async () => {
    for (const [title, icon] of [
        ['Entrer en mode plein écran', FULLSCREEN_PATHS[0]],
        ['Quitter le mode plein écran', FULLSCREEN_PATHS[1]],
        ['Vollbildmodus aktivieren', FULLSCREEN_PATHS[0]],
        ['Vollbildmodus verlassen', FULLSCREEN_PATHS[1]]
    ]) {
        const p = await player('fr-FR');
        p.fullscreen(title, icon, false);
        assert.equal(p.findFullscreen(), null);
        const control = p.fullscreen(title, icon);
        assert.equal(p.findFullscreen(), control);
        p.refresh();
        assert.equal(p.settingsButton().nextElementSibling, control);
    }
    const p = await player();
    const control = p.fullscreen('Enter fullscreen mode', 'unrecognized icon');
    assert.equal(p.findFullscreen(), control);
});

test('mounts a styled manual offer without an anchor, including before the body exists', async () => {
    const p = await player('de-DE', { bodyReady: false });
    p.offer('offer-before-body');
    p.notify('intro');
    assert.equal(p.skipButton(), undefined);
    p.ready();
    assert.equal(p.hasStyles(), true);
    assert.equal(p.skipButton().hidden, false);
    assert.equal(p.toast().textContent, 'Opening skipped');
    assert.equal(p.settingsButton(), undefined);
    const control = p.fullscreen('Vollbildmodus aktivieren', FULLSCREEN_PATHS[0]);
    p.refresh();
    assert.equal(p.settingsButton().nextElementSibling, control);
    p.skipButton().click();
    assert.deepEqual(p.posts.at(-1).args, ['shell-manual-skip', { offer_id: 'offer-before-body' }]);
    assert.equal(p.skipButton().hidden, true);
});

test('skip controls leave playback focus alone on mouse activation', async () => {
    const p = await player();
    p.fullscreen('Enter fullscreen mode', FULLSCREEN_PATHS[0]);
    p.refresh();
    p.pointerDown(p.settingsButton());
    p.settingsButton().click();
    assert.equal(p.activeElement(), undefined);
    p.offer('clickable-offer');
    p.pointerDown(p.skipButton());
    p.skipButton().click();
    assert.equal(p.activeElement(), undefined);
    assert.deepEqual(p.posts.at(-1).args, ['shell-manual-skip', { offer_id: 'clickable-offer' }]);
    assert.equal(p.keydown('ArrowLeft').stopped, false);
});

test('closing skip settings restores focus without swallowing repeated seek shortcuts', async () => {
    const p = await player();
    p.fullscreen('Enter fullscreen mode', FULLSCREEN_PATHS[0]);
    p.refresh();
    p.settingsButton().click(0);
    assert.equal(p.activeElement().name, 'shell-intro-mode');
    const radioArrow = p.keydown('ArrowRight');
    assert.equal(radioArrow.stopped, true);
    assert.equal(radioArrow.defaultPrevented, false);
    const escape = p.keydown('Escape');
    assert.equal(escape.defaultPrevented, true);
    assert.equal(p.activeElement(), p.settingsButton());
    for (const key of ['ArrowLeft', 'ArrowLeft', 'ArrowRight', 'ArrowRight', 'f']) {
        assert.equal(p.keydown(key).stopped, false, `${key} must reach playback`);
    }
    for (const key of ['Enter', ' ', 'Tab']) {
        const activation = p.keydown(key);
        assert.equal(activation.stopped, true);
        assert.equal(activation.defaultPrevented, false);
    }
});

test('a focused manual skip keeps native activation and allows playback shortcuts', async () => {
    const p = await player();
    p.offer('keyboard-offer');
    p.skipButton().focus();
    assert.equal(p.keydown('ArrowLeft').stopped, false);
    assert.equal(p.keydown('ArrowRight').stopped, false);
    const activation = p.keydown(' ');
    assert.equal(activation.stopped, true);
    assert.equal(activation.defaultPrevented, false);
});

test('only submits the current offer and clears it on timeout, hide and navigation', async () => {
    const p = await player();
    const skipPosts = () => p.posts.filter(post => post.args[0] === 'shell-manual-skip');
    p.offer(null);
    assert.equal(p.skipButton().hidden, true);
    p.offer('expired');
    p.advance(8000);
    p.skipButton().click();
    assert.equal(skipPosts().length, 0);
    p.offer('hidden');
    p.offer(null, false);
    p.skipButton().click();
    assert.equal(skipPosts().length, 0);
    p.offer('old-route');
    p.navigate('#/player/two');
    p.skipButton().click();
    assert.equal(skipPosts().length, 0);
    p.offer('current');
    p.skipButton().click();
    p.skipButton().click();
    assert.deepEqual(skipPosts().map(post => post.args[1]), [{ offer_id: 'current' }]);
    p.offer('replacement');
    p.offer('current', false);
    assert.equal(p.skipButton().hidden, false);
    p.skipButton().click();
    assert.deepEqual(skipPosts().at(-1).args[1], { offer_id: 'replacement' });
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
