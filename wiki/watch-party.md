# Watch Party Guide

Watch Party lets one person host playback and share a Discord Rich Presence join button so friends can sync their Stremio playback through Steam networking.

## Requirements

- An updated Windows x64 Steam-enabled Stremio Shell NG from this fork on every participant's computer (watch-party protocol 2). The ARM64 desktop build currently omits Steam networking.
- Steam installed, running, and signed in before starting or joining a watch party
- Discord running if you want friends to join from your Rich Presence button

## Host A Watch Party

1. Open Steam and make sure you are signed in.
2. Open Stremio Shell NG.
3. Start playing the movie or episode you want to watch.
4. Right-click the Stremio tray icon.
5. Select **Start Watch Party**.
6. Wait for the watch-party notification.
7. Friends can join from the button shown on your Discord Rich Presence.

![Start Watch Party](https://i.imgur.com/D0oWZ2G.png)

![Watch Party Notification](https://i.imgur.com/U6vMdFi.png)

## Let Each Viewer Choose Their Own Stream

Before starting the party, select **Use each viewer's own stream** in the tray menu. This choice is saved beside the executable in `RPCconfig.ini`:

```ini
[WatchParty]
source_mode = local
```

The host shares the movie/episode identity and playback state. Guests open that episode's normal stream selector and choose a source through their own installed addons and account. Their selected URLs stay local. Once their video has loaded, playback joins the host's current position and follows play, pause and seek changes. When the host changes episodes, guests select a source for the new episode again.

If the host reloads or changes source for the same episode, guests pause while it loads, keep their chosen source, and catch up when it is ready. Delayed updates for an earlier episode cannot seek the current video.

Choose the same release or edition as the host whenever possible. Different cuts can have different timings even when the episode ID matches. Each guest needs their own usable source; this mode cannot resolve an arbitrary host HTTP URL into an episode. Start the host's movie or episode through its Stremio details page so metadata is available.

The choice can be changed when the party has ended. With the option unchecked (`source_mode = shared`, the default), guests use the host's shared source. Local mode does not fall back to that URL if selection or loading fails.

Older watch-party clients and protocol-1 invitation links must be updated. They are rejected rather than silently using shared-source playback.

## Join A Watch Party

1. Open Steam and make sure you are signed in.
2. Open Discord.
3. Find your friend's Stremio Rich Presence.
4. Click the join button.
5. Stremio Shell NG should open or focus and connect to the host.
6. In local-source mode, choose a stream on the episode's details page. In shared-source mode, playback loads the host's source. Synchronization starts when the selected video is loaded.

![Join Button](https://i.imgur.com/UzJuzFS.png)

![Joining Watch Party](https://i.imgur.com/3bzwHrn.png)

## During Playback

- The host controls the shared playback state.
- Guests follow host playback updates such as loading, play, pause, and seeking.
- The window name and the overlay shows whether you are hosting or joined.
- The host can end the watch party from the tray menu.
- Guests can leave the watch party from the tray menu or in-app overlay.


## Troubleshooting

### Steam Is Not Running

If Steam is closed, Stremio Shell NG will show an error when you try to start or join a watch party. Open Steam, sign in, then try again. You may also need to add Spacewar to your Steam library if you haven't already, as it's used for the underlying networking.


## How to Add Spacewar to Your Steam Library

1. Use the shortcut Windows key + R
2. Right click the start button and select Run
3. Go to the start menu and search for Run
4. Type `steam://run/480` and click OK. This command will attempt to run Spacewar (application code being 480), and since it is not installed, it will prompt to install it.
5. You can cancel the installation if you don't want to actually install the game, but it needs to be added to your library for the watch party feature to work.

### Discord Join Button Does Not Appear

Make sure Discord is running and Rich Presence is enabled in your configuration. If you recently started Discord, wait a few seconds or restart playback so the activity can refresh.

### Friend Cannot Join

Check that both people have Steam open and signed in. The host should keep Stremio running and should not end the party before the guest joins.

### Playback Does Not Load

The guest needs access to the same Stremio content and stream source. If an addon, stream, or region-specific source is unavailable for the guest, syncing can connect but playback may not load correctly.

## Ending A Watch Party

- Host: right-click the tray icon and select **End Watch Party**.
- Guest: right-click the tray icon and select **Leave Watch Party**, or use the leave button in the watch-party overlay.
