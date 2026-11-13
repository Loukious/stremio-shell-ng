(function() {
const state = __STREMIO_WATCH_PARTY_STATE__;
const id = "stremio-shell-watch-party";
let el = document.getElementById(id);
if (!state.visible) {
    if (el) {
        clearTimeout(el.__stremioWatchPartyTimer);
        el.remove();
    }
    return;
}
if (!el) {
    el = document.createElement("div");
    el.id = id;
    document.documentElement.appendChild(el);
}
if (!el.querySelector('[data-role="panel"]')) {
    el.style.cssText = [
        "position:fixed",
        "left:50%",
        "top:0",
        "transform:translateX(-50%)",
        "z-index:2147483647",
        "width:min(420px,calc(100vw - 40px))",
        "height:142px",
        "pointer-events:none"
    ].join(";");
    el.innerHTML = `
        <div data-role="hotspot" style="position:absolute;left:50%;top:0;transform:translateX(-50%);width:270px;height:30px;pointer-events:auto"></div>
        <div data-role="panel" style="
            position:absolute;
            left:50%;
            top:12px;
            transform:translate(-50%,-10px);
            width:100%;
            box-sizing:border-box;
            opacity:0;
            transition:opacity .16s ease,transform .16s ease;
            pointer-events:none;
            font:13px/1.35 system-ui,-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;
            color:#f5f7fb;
            background:linear-gradient(135deg,rgba(19,22,34,.82),rgba(30,22,52,.74));
            border:1px solid rgba(142,105,255,.28);
            box-shadow:0 18px 60px rgba(0,0,0,.34),0 0 28px rgba(126,87,255,.12) inset;
            backdrop-filter:blur(18px);
            border-radius:8px;
            padding:11px 12px;
            overflow:hidden">
            <div style="display:flex;gap:10px;align-items:flex-start">
                <div style="width:9px;height:9px;border-radius:50%;background:#8b5cf6;box-shadow:0 0 12px rgba(139,92,246,.9);margin-top:5px;flex:0 0 auto"></div>
                <div style="min-width:0;flex:1">
                    <div data-role="badge" style="font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"></div>
                    <div data-role="event" style="margin-top:3px;color:rgba(226,232,240,.78);overflow-wrap:anywhere"></div>
                </div>
                <button data-role="leave" style="display:none;border:1px solid rgba(255,255,255,.12);border-radius:6px;background:rgba(255,255,255,.08);color:#f8fafc;padding:6px 9px;font:inherit;cursor:pointer;flex:0 0 auto">Leave</button>
            </div>
            <div data-role="members" style="display:grid;grid-template-columns:1fr;gap:5px;margin-top:10px;max-height:86px;overflow:auto"></div>
        </div>
    `;
    const panel = el.querySelector('[data-role="panel"]');
    const show = () => {
        panel.style.opacity = "1";
        panel.style.transform = "translate(-50%,0)";
        panel.style.pointerEvents = "auto";
    };
    const hide = () => {
        if (el.matches(":hover")) return;
        panel.style.opacity = "0";
        panel.style.transform = "translate(-50%,-10px)";
        panel.style.pointerEvents = "none";
    };
    el.querySelector('[data-role="hotspot"]').addEventListener("mouseenter", show);
    panel.addEventListener("mouseenter", show);
    el.addEventListener("mouseleave", () => setTimeout(hide, 120));
    el.__stremioWatchPartyShow = show;
    el.__stremioWatchPartyHide = hide;
    el.querySelector('[data-role="leave"]').addEventListener("click", () => {
        try {
            window.chrome.webview.postMessage(JSON.stringify({
                id: 1,
                args: ["shell-watch-party-leave"]
            }));
        } catch (_) {}
    });
}
el.querySelector('[data-role="badge"]').textContent = state.badge || "Watch Party";
const event = el.querySelector('[data-role="event"]');
const readinessKey = JSON.stringify(state.readinessKey || null);
const messageIsCurrent = (!state.eventReadinessKey || JSON.stringify(state.eventReadinessKey) === readinessKey)
    && (state.eventReadinessSequence == null || state.eventReadinessSequence === state.readinessSequence);
const message = messageIsCurrent
    ? [state.eventTitle, state.eventText].filter(Boolean).join(": ") : "";
const baseline = state.active
    ? (state.readinessActive ? "Each viewer chooses their own stream." : "Playback sync is active.")
    : "";
if (el.__stremioWatchPartyReadinessKey !== readinessKey
    || (el.__stremioWatchPartyMessageSequence != null
        && el.__stremioWatchPartyMessageSequence !== state.readinessSequence)) {
    // A changed episode or source status must not retain a previous ready notice.
    el.__stremioWatchPartyReadinessKey = readinessKey;
    el.__stremioWatchPartyMessage = "";
    el.__stremioWatchPartyMessageSequence = null;
    clearTimeout(el.__stremioWatchPartyTimer);
    if (el.__stremioWatchPartyHide) el.__stremioWatchPartyHide();
}
if (message) {
    el.__stremioWatchPartyMessage = message;
    el.__stremioWatchPartyMessageSequence = state.eventReadinessSequence;
}
event.textContent = el.__stremioWatchPartyMessage || baseline;
const leave = el.querySelector('[data-role="leave"]');
leave.style.display = state.canLeave ? "block" : "none";
const members = el.querySelector('[data-role="members"]');
members.innerHTML = "";
const memberColumns = "6px minmax(0,1fr)" + (state.readinessActive ? " 96px" : "")
    + (state.canKick ? " 24px" : "");
(state.members || []).forEach((member) => {
    const row = document.createElement("div");
    row.style.cssText = `display:grid;grid-template-columns:${memberColumns};align-items:center;gap:8px;min-width:0;color:rgba(241,245,249,.9)`;
    const dot = document.createElement("div");
    dot.style.cssText = `width:6px;height:6px;border-radius:50%;flex:0 0 auto;background:${member.isHost ? "#8b5cf6" : "#38bdf8"}`;
    const name = document.createElement("div");
    name.textContent = `${member.name || "Unknown"}${member.isHost ? " - Host" : ""}`;
    name.style.cssText = "min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis";
    row.appendChild(dot);
    row.appendChild(name);
    if (state.readinessActive) {
        const readiness = {
            selecting: ["Selecting source", "#7dd3fc"],
            loading: ["Loading video", "#fcd34d"],
            ready: ["Ready", "#86efac"]
        }[member.readiness] || ["Unknown", "#94a3b8"];
        const status = document.createElement("span");
        status.setAttribute("data-readiness", member.readiness || "unknown");
        status.textContent = readiness[0];
        status.style.cssText = `grid-column:3;justify-self:start;white-space:nowrap;font-size:11px;font-weight:600;color:${readiness[1]}`;
        row.appendChild(status);
    }
    if (state.canKick && !member.isHost) {
        const kick = document.createElement("button");
        kick.type = "button";
        kick.textContent = "×";
        const removeLabel = `Remove ${member.name || "viewer"} from watch party`;
        kick.setAttribute("title", removeLabel);
        kick.setAttribute("aria-label", removeLabel);
        kick.style.cssText = `grid-column:${state.readinessActive ? 4 : 3};display:grid;place-items:center;width:24px;height:24px;padding:0;border:1px solid rgba(255,255,255,.12);border-radius:6px;background:rgba(239,68,68,.12);color:#fecaca;font:20px/1 system-ui,sans-serif;cursor:pointer;outline-offset:2px`;
        kick.addEventListener("click", () => {
            try {
                window.chrome.webview.postMessage(JSON.stringify({
                    id: 1,
                    args: ["shell-watch-party-kick", { steamId: String(member.steamId) }]
                }));
            } catch (_) {}
        });
        row.appendChild(kick);
    }
    members.appendChild(row);
});
if (message && el.__stremioWatchPartyShow) {
    el.__stremioWatchPartyShow();
    clearTimeout(el.__stremioWatchPartyTimer);
    el.__stremioWatchPartyTimer = setTimeout(() => {
        el.__stremioWatchPartyMessage = "";
        el.__stremioWatchPartyMessageSequence = null;
        event.textContent = baseline;
        if (el.__stremioWatchPartyHide) el.__stremioWatchPartyHide();
    }, 4200);
}
})();
