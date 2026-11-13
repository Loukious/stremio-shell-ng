const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, 'watch_party_overlay.js'), 'utf8');

function overlay() {
    const timers = new Map(), posts = [];
    let now = 0, timerId = 0;
    class Element {
        constructor(tag) {
            this.tagName = tag.toUpperCase();
            this.children = [];
            this.attributes = new Map();
            this.listeners = {};
            this.style = {};
            this.textContent = '';
        }
        setAttribute(name, value) { this.attributes.set(name, String(value)); }
        getAttribute(name) { return this.attributes.get(name); }
        appendChild(child) { child.remove(); this.children.push(child); child.parentElement = this; return child; }
        remove() {
            if (this.parentElement) this.parentElement.children.splice(this.parentElement.children.indexOf(this), 1);
            this.parentElement = null;
        }
        addEventListener(name, listener) { (this.listeners[name] ||= []).push(listener); }
        fire(name) { for (const listener of this.listeners[name] || []) listener(); }
        matches(selector) {
            const attribute = selector.match(/^\[([^=]+)="([^"]+)"\]$/);
            return attribute ? this.getAttribute(attribute[1]) === attribute[2] : false;
        }
        querySelector(selector) {
            for (const child of this.children) {
                if (child.matches(selector)) return child;
                const nested = child.querySelector(selector);
                if (nested) return nested;
            }
            return null;
        }
        set innerHTML(value) {
            for (const child of this.children) child.parentElement = null;
            this.children = [];
            // Parse only the static scaffold. Dynamic participant values must
            // reach textContent and never enter this HTML path.
            for (const match of value.matchAll(/<(\w+)[^>]*\bdata-role="([^"]+)"[^>]*>/g)) {
                const child = this.appendChild(new Element(match[1]));
                child.setAttribute('data-role', match[2]);
                if (/opacity:0/.test(match[0])) child.style.opacity = '0';
            }
        }
    }
    const document = {
        documentElement: new Element('html'),
        createElement: tag => new Element(tag),
        getElementById: id => document.documentElement.children.find(element => element.id === id)
    };
    const context = vm.createContext({
        document,
        window: { chrome: { webview: { postMessage: message => posts.push(JSON.parse(message)) } } },
        setTimeout: (callback, delay) => {
            const id = ++timerId;
            timers.set(id, { callback, at: now + delay });
            return id;
        },
        clearTimeout: id => timers.delete(id)
    });
    return {
        render: state => vm.runInContext(source.replace('__STREMIO_WATCH_PARTY_STATE__', JSON.stringify(state)), context),
        panel: () => document.getElementById('stremio-shell-watch-party'),
        role: role => document.getElementById('stremio-shell-watch-party')?.querySelector(`[data-role="${role}"]`),
        posts,
        timerCount: () => timers.size,
        advance: milliseconds => {
            now += milliseconds;
            for (const [id, timer] of timers) {
                if (timer.at <= now) { timers.delete(id); timer.callback(); }
            }
        }
    };
}

function state(overrides = {}) {
    return {
        active: true, visible: true, badge: 'Joined watch party 4/8', canLeave: true, canKick: false,
        readinessActive: true,
        readinessSequence: '1',
        readinessKey: [{ content_type: 'series', metadata_id: 'tt1234567', video_id: 'tt1234567:1:1' }, 1],
        members: [
            { steamId: '1', name: 'Host', isHost: true, readiness: 'ready' },
            { steamId: '2', name: 'Alice', readiness: 'selecting' },
            { steamId: '3', name: 'Bob', readiness: 'loading' },
            { steamId: '4', name: 'Carol', readiness: null }
        ],
        eventTitle: null, eventText: null,
        ...overrides
    };
}

test('shows distinct source selection, loading, ready and unknown states to every viewer', () => {
    for (const canKick of [false, true]) {
        const view = overlay();
        view.render(state({ canKick }));
        const rows = view.role('members').children;
        assert.deepEqual(rows.map(row => row.children[2].textContent), ['Ready', 'Selecting source', 'Loading video', 'Unknown']);
        assert.deepEqual(rows.map(row => row.children[2].getAttribute('data-readiness')), ['ready', 'selecting', 'loading', 'unknown']);
        assert.equal(view.role('event').textContent, 'Each viewer chooses their own stream.');
        assert.equal(view.role('panel').style.opacity, '0');
    }
});

test('ready announcement survives roster refreshes without extending or reopening its timeout', () => {
    const view = overlay();
    view.render(state({ eventTitle: 'Watch Party', eventText: 'Alice is ready.', eventReadinessKey: state().readinessKey, eventReadinessSequence: '1' }));
    assert.equal(view.role('panel').style.opacity, '1');
    view.advance(2000);
    view.render(state());
    assert.equal(view.role('event').textContent, 'Watch Party: Alice is ready.');
    view.advance(2199);
    assert.equal(view.role('panel').style.opacity, '1');
    view.advance(1);
    assert.equal(view.role('panel').style.opacity, '0');
    assert.equal(view.role('event').textContent, 'Each viewer chooses their own stream.');
    view.render(state());
    assert.equal(view.role('panel').style.opacity, '0');
    assert.equal(view.timerCount(), 0);
});

test('changing the episode clears a previous ready notice and replaces all member statuses', () => {
    const view = overlay();
    view.render(state({ eventText: 'Alice is ready.' }));
    view.render(state({
        readinessKey: [{ content_type: 'series', metadata_id: 'tt1234567', video_id: 'tt1234567:1:2' }, 2],
        members: [{ steamId: '2', name: 'Alice', readiness: 'selecting' }]
    }));
    assert.equal(view.role('event').textContent, 'Each viewer chooses their own stream.');
    assert.equal(view.role('members').children.length, 1);
    assert.equal(view.role('members').children[0].children[2].textContent, 'Selecting source');
    assert.equal(view.role('panel').style.opacity, '0');
    assert.equal(view.timerCount(), 0);
});

test('shared playback and cleared metadata do not retain local readiness badges', () => {
    const view = overlay();
    view.render(state({ eventText: 'Alice is ready.' }));
    view.render(state({ readinessActive: false, readinessKey: null }));
    assert.equal(view.role('event').textContent, 'Playback sync is active.');
    assert.ok(view.role('members').children.every(row => row.children.length === 2));
    assert.equal(view.timerCount(), 0);
});

test('delayed readiness announcements cannot reopen the panel for another episode or generation', () => {
    const view = overlay();
    const old = state().readinessKey;
    for (const current of [
        [{ content_type: 'series', metadata_id: 'tt1234567', video_id: 'tt1234567:1:2' }, 2],
        [old[0], 3]
    ]) {
        view.render(state({ readinessKey: current }));
        view.render(state({
            readinessKey: current, eventReadinessKey: old,
            eventTitle: 'Watch Party', eventText: 'Alice is ready.'
        }));
        assert.equal(view.role('event').textContent, 'Each viewer chooses their own stream.');
        assert.equal(view.role('panel').style.opacity, '0');
        assert.equal(view.timerCount(), 0);
    }
});

test('same-episode source reload clears and rejects older ready notices while keeping the current loading status', () => {
    const view = overlay();
    const scope = state().readinessKey;
    view.render(state({
        eventTitle: 'Watch Party', eventText: 'Alice is ready.',
        eventReadinessKey: scope, eventReadinessSequence: '1'
    }));
    const loading = state({
        readinessSequence: '2',
        members: [{ steamId: '2', name: 'Alice', readiness: 'loading' }]
    });
    view.render(loading);
    view.render({
        ...loading, eventTitle: 'Watch Party', eventText: 'Alice is ready.',
        eventReadinessKey: scope, eventReadinessSequence: '1'
    });
    assert.equal(view.role('members').children[0].children[2].textContent, 'Loading video');
    assert.equal(view.role('event').textContent, 'Each viewer chooses their own stream.');
    assert.equal(view.role('panel').style.opacity, '0');
    assert.equal(view.timerCount(), 0);
    view.render({
        ...loading, eventTitle: 'Watch Party', eventText: 'Alice is loading the video.',
        eventReadinessKey: scope, eventReadinessSequence: '2'
    });
    assert.equal(view.role('event').textContent, 'Watch Party: Alice is loading the video.');
    assert.equal(view.role('panel').style.opacity, '1');
});

test('participant text stays literal and existing leave and remove controls preserve their messages', () => {
    const view = overlay();
    view.render(state({ canKick: true, members: [{ steamId: '42', name: '<img src=x onerror=bad()>', readiness: 'ready' }] }));
    const row = view.role('members').children[0];
    assert.equal(row.children[1].textContent, '<img src=x onerror=bad()>');
    assert.equal(row.children[1].children.length, 0);
    assert.equal(row.children[3].textContent, '×');
    assert.equal(row.children[3].getAttribute('title'), 'Remove <img src=x onerror=bad()> from watch party');
    assert.equal(row.children[3].getAttribute('aria-label'), 'Remove <img src=x onerror=bad()> from watch party');
    row.children[3].fire('click');
    view.role('leave').fire('click');
    assert.deepEqual(view.posts.map(post => post.args), [
        ['shell-watch-party-kick', { steamId: '42' }], ['shell-watch-party-leave']
    ]);
});

test('leaving removes the panel and cancels pending announcements', () => {
    const view = overlay();
    view.render(state({ eventText: 'Alice is ready.' }));
    assert.equal(view.timerCount(), 1);
    view.render(state({ visible: false, active: false, readinessKey: null }));
    assert.equal(view.panel(), undefined);
    assert.equal(view.timerCount(), 0);
    view.advance(5000);
    assert.equal(view.panel(), undefined);
});
