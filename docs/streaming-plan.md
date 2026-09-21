# Fly-plays-Game-Boy: 24/7 Twitch streaming architecture and action plan

Draft 2026-09-15. Research only: no repo changes, no VMs created, the host untouched.

Goal: two persistent demos, each in its own VM on the host (Proxmox, no GPU), each streaming 24/7 to its own Twitch channel. Demo 1 is the existing `~/fly-plays-pokemon` (Pokemon Red). Demo 2 is a Game Boy platformer.

## 1. What the existing app constrains

- Runtime is a browser page. `src/simulation.worker.ts` owns binjgb WASM plus the 139,255-neuron / 2,700,513-edge LIF kernel; `src/main.ts` renders snapshots and holds the server lease.
- `src/view/connectome.ts` uses `THREE.WebGLRenderer`. This is the biggest constraint: the neural map needs a real WebGL2 context, so any browser-free rewrite must also replace the renderer.
- Persistence is server-side. `server/session-store.ts` is mounted in both `configureServer` and `configurePreviewServer` (`vite.config.ts`), so `npm run build && npm run preview` gives the full checkpoint API without the dev server. Port 7377 strict, `POKEMON_SAVE_DIR` defaults to `local/saves`.
- Exactly one Node process per save dir, exactly one browser lease, 15 s lease TTL, 4 s heartbeat, 5 s autosave (`docs/server-sessions.md`). A stalled page loses the lease and the worker stops, so a browser watchdog is mandatory. Conveniently, "restart Chromium" is the designed recovery path: any heartbeat/upload failure already terminates the worker and requires a fresh acquire plus server restore.
- Measured ~20.9 emulator fps under Playwright on the 5800X3D WSL box (`docs/architecture.md`), versus 59.7 fps Game Boy real time. The stream will show slow-motion gameplay. Accept it.
- Reward adapter seam: `src/reward/pokemon-red.ts` exports `interface MemoryReader { read8(address: number): number }`, `REWARD_ADAPTER`, `SUPPORTED_ROM` and `sample(source, brainMs)`; `src/reward/catalog.ts` holds `REWARDS`. A second game is a sibling module plus sibling catalog plus a bumped compatibility string in `src/runtime/compatibility.ts` (which today pins the binjgb revision and `POKERED_COMMIT`).
- `Binjgb.read8` calls `_emulator_read_mem`, a CPU-bus read. Relevant to demo 2, see section 6.

## 2. Headless capture: comparison and recommendation

### (a) Xvfb + headful Chromium + ffmpeg x11grab. RECOMMENDED

Xvfb at 1280x720x24, Chromium started **headful** (not `--headless`) in kiosk mode on that display, ffmpeg `-f x11grab` pushing H.264/AAC FLV to Twitch. Standard shape for 24/7 browser streams ([xvfb-record](https://github.com/grayleonard/xvfb-record), [lofi-style 24/7 writeup](https://medium.com/@EsteveSegura/how-to-stream-to-twitch-24-7-like-lofi-girl-youtube-channel-for-less-than-8-month-dca5bc662912)).

Why it wins: the page is genuinely visible, so hidden-tab throttling never engages; ffmpeg owns the output clock, so `-framerate 30 -r 30` yields CFR to Twitch even though the app paints at ~20 fps; ffmpeg is one long-lived restartable process with machine-readable `-progress`; and WebGL works via SwiftShader (2.5). The visibility claim is reasoned from X11 occlusion semantics, not a cited doc: treat as **unverified** until Phase 0 logs `document.visibilityState` and rAF cadence for an hour.

Flag set, all documented in [chrome-flags-for-tools.md](https://github.com/GoogleChrome/chrome-launcher/blob/main/docs/chrome-flags-for-tools.md):

```
--disable-background-timer-throttling      # timers not throttled in background pages
--disable-backgrounding-occluded-windows   # occluded window not treated as backgrounded
--disable-renderer-backgrounding           # no lower priority for non-foreground tabs
--disable-ipc-flooding-protection          # default caps IPC at 10/s/frame
--disable-hang-monitor --hide-scrollbars --mute-audio
--autoplay-policy=no-user-gesture-required
--disable-dev-shm-usage                    # small /dev/shm in minimal VMs
--kiosk --window-position=0,0 --window-size=1280,720
--no-first-run --no-default-browser-check --disable-features=Translate
```

Chromium throttles background-page timers to about 1 Hz ([Intent to Ship: Expensive Background Timer Throttling](https://groups.google.com/a/chromium.org/g/blink-dev/c/XRqy8mIOWps)); we do not rely on the flags alone, we keep the page foreground.

### (b) `--headless` + CDP `Page.startScreencast` piped to ffmpeg. REJECT

Screencast emits JPEG frames with no rate guarantee. Upstream calls it unsuitable: "doesn't scale well and yields a poor-quality video" ([crbug 781117](https://bugs.chromium.org/p/chromium/issues/detail?id=781117)), `everyNthFrame` is "not good enough because web pages can be very slow" ([devtools-protocol#63](https://github.com/ChromeDevTools/devtools-protocol/issues/63)), and it is reported as extremely CPU heavy ([testcafe#6422](https://github.com/DevExpress/testcafe/issues/6422)). We would spend cores on JPEG encode then decode then x264, on a box whose cores belong to the LIF kernel. Aside: since Chrome 132 plain `--headless` is the new implementation, so `--headless=new` is redundant (chrome-flags-for-tools above).

### (c) Rewrite to pure Node, render with node-canvas, pipe rawvideo. REJECT FOR PHASE 1, KEEP AS PHASE 4

Tempting: the sim is pure TS in a worker and binjgb is WASM, so the sim and the 160x144 framebuffer would port cleanly, dropping Chromium, Xvfb, SwiftShader and the lease protocol. Blocker: the connectome map is `THREE.WebGLRenderer`, so Node needs `headless-gl` or a rewrite of the map to 2D canvas or a software point rasteriser over 139k points. That is real work, it changes the stream's visual identity, and every `tests/browser` case would need a parallel story. Do not block going live on it.

### (d) OBS in a VM with browser source + obs-websocket. REJECT

OBS on Linux requires a GPU with at least OpenGL 3.3 ([OBS forums](https://obsproject.com/forum/threads/for-those-migrating-old-windows-10-pcs-to-linux-why-obs-wont-work-but-can-work.182848/)). With no GPU that means Mesa llvmpipe via `MESA_LOADER_DRIVER_OVERRIDE=llvmpipe` ([headless OBS thread](https://obsproject.com/forum/threads/best-solution-for-headless-obs-on-linux.169182/)), plus an embedded CEF browser, plus an X server anyway. Strictly more software than (a) for the same result, and it burns CPU compositing. Keep OBS on the WSL box for one-off manual scene design only.

### 2.5 WebGL without a GPU

Chromium removed the automatic SwiftShader WebGL fallback starting in Chrome 130; you must opt in ([chromium docs/gpu/swiftshader.md](https://chromium.googlesource.com/chromium/src/+/main/docs/gpu/swiftshader.md), [EnableUnsafeSwiftShader policy](https://chromeenterprise.google/policies/enable-unsafe-swift-shader/)). Add `--enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader`. We render only local content, so the "not for untrusted content" caveat is acceptable. **Risk:** SwiftShader on 139k points with a custom `ShaderMaterial` at 720p is unbenchmarked. Phase 0 must measure it; the fallback is to shrink the map canvas and `setPixelRatio(1)`, not to abandon the design.

### 2.6 CPU budget (estimates, not measurements)

| Component | Estimate |
| --- | --- |
| LIF kernel worker (2.7M edges x 16-17 ms steps per GB frame) | 1 core saturated; this is why fps is 20 not 60 |
| binjgb WASM + framebuffer copy | 0.2 core |
| Chromium renderer + SwiftShader connectome | 1 to 2 cores, **unverified** |
| Xvfb | 0.2 core |
| ffmpeg x11grab + libx264 `veryfast` 720p30 | 1 to 1.5 cores |
| same with `ultrafast` | 0.5 to 1 core |
| Node preview server + checkpoint fsync | 0.1 core |

Allocate **8 vCPU / 8 GB** per demo VM, floor 6 vCPU. Start at `veryfast`; drop to `ultrafast` only if sim fps regresses, since ultrafast costs visible quality at 3000 kbps. If the encoder starves the sim, use `CPUWeight=`/`CPUQuota=` on the ffmpeg unit rather than hand-pinning.

Host headroom: two such VMs demand ~16 vCPU of burst. The host's core count is **unverified** here — the operator's infra repo is not checked out on this box, and the notes that are records only the PVE version and one guest's sizing. Confirm `lscpu` / `free -g` on the host and check the headroom against everything else already running on it: another service's production container, the monitoring and panel containers, and a couple of other guests.

## 3. Twitch specifics

**Ingest.** Verified live: `GET https://ingest.twitch.tv/ingests` returns an `ingests` array with `_id`, `availability`, `default`, `name`, `url_template`, `url_template_secure`, `priority`. Templates fetched 2026-09-15: `rtmp://ingest.global-contribute.live-video.net/app/{stream_key}`, `rtmp://use10.contribute.live-video.net/app/{stream_key}`, `rtmp://use20.contribute.live-video.net/app/{stream_key}`. Docs: [Get Ingest Servers](https://dev.twitch.tv/docs/video-broadcast/reference/); human picker: https://stream.twitch.tv/ingests/. Use the global endpoint unless a probe says otherwise, and prefer the `url_template_secure` RTMPS form.

**Encoder settings.** Twitch recommends 720p30 at 3000 kbps or 720p60 at 4500 kbps, CBR, 2 s keyframe interval, and states 2 s keyframes are required rather than optional ([Broadcasting Guidelines](https://help.twitch.tv/s/article/broadcasting-guidelines?language=en_US), via [keyframe guide](https://alive-project.com/en/streamer-magazine/article/5755/) and [Switchboard KB](https://kb.switchboard.live/articles/360033880534-switchboard-and-twitch-recommended-encoder-settings)). The help page is JS-rendered and did not fetch cleanly, so treat the exact wording as second-hand.

Choose **720p30, not 1080p**: the Game Boy source is 160x144 nearest-neighbour upscaled so 1080p buys nothing on the game panel, the connectome canvas is the only detailed surface and it is software-rendered, and 1080p roughly doubles encoder CPU we cannot spare. Non-partner channels usually get no transcodes, so a lower bitrate is friendlier to viewers too.

Twitch needs an audio track; video-only RTMP is a known failure source and the fix is a generated silent track ([ffmpeg-to-RTMP writeup](https://www.codestudy.net/blog/stream-mp4-video-successfully-to-rtmp-with-ffmpeg/)). Use H.264 + AAC + `yuv420p` + `flv`. Reference command (untested, Phase 0 validates):

```
ffmpeg -loglevel warning -nostdin \
  -f x11grab -framerate 30 -video_size 1280x720 -draw_mouse 0 -i :99 \
  -f lavfi -i anullsrc=channel_layout=stereo:sample_rate=44100 \
  -c:v libx264 -preset veryfast -tune zerolatency -pix_fmt yuv420p \
  -b:v 3000k -minrate 3000k -maxrate 3000k -bufsize 6000k \
  -g 60 -keyint_min 60 -sc_threshold 0 -r 30 \
  -c:a aac -b:a 160k -ar 44100 -progress unix:///run/fly-stream/progress.sock \
  -f flv "rtmps://ingest.global-contribute.live-video.net/app/$TWITCH_KEY"
```

`-g 60 -keyint_min 60 -sc_threshold 0` is the 2 s keyframe requirement at 30 fps with scene-cut keyframes disabled.

**Stream key handling.** Never in git, never in a CLAUDE.md, never in a compose file. Two acceptable patterns: (1) `LoadCredentialEncrypted=twitch-key:/etc/fly-stream/twitch-key.cred` built with `systemd-creds encrypt`, read at start from `$CREDENTIALS_DIRECTORY/twitch-key`; or (2) `EnvironmentFile=/etc/fly-stream/stream.env`, mode 0600 root-only, holding `TWITCH_KEY=...`. Source of truth is `pass` on the WSL box, matching the naming pattern the operator already uses for other infrastructure entries: suggest `twitch/<channel>-key` and `twitch/<platformer-channel>-key`. A leaked key lets anyone broadcast as you, so rotate immediately if it ever lands in a log.

**48 hour limit.** Twitch's maximum broadcast length is 48 hours and cannot be changed; to run longer you end and immediately restart the broadcast ([Broadcasting Guidelines](https://help.twitch.tv/s/article/broadcasting-guidelines?language=en_US), as quoted by [streamersplaybook](https://streamersplaybook.com/how-long-can-you-stream-on-twitch-for/) and [ireplay](https://ireplay.tv/blog/24-7-always-on-streaming-twitch-grow-audience-with-existing-content/)). Reported behaviour is that Twitch cuts the stream at 48 h while the channel keeps its followers and schedule, and 24/7 channels simply restart before the cap (ireplay). VODs are capped at 48 h and retained 7/14/60 days by tier ([Restream](https://support.restream.io/en/articles/9096867-how-long-can-i-stream-for)); highlight/upload storage is capped at 100 hours ([TechCrunch](https://techcrunch.com/2025/02/20/twitch-caps-streamers-storage-at-100-hours-of-highlights-and-uploads)).

Design consequence: a systemd timer restarts **only the ffmpeg unit** every 23 hours. That leaves comfortable margin, produces one tidy VOD per day, and does not touch the browser or the simulation, so the fly keeps playing across the seam. Viewers see a short reconnect. Twitch's exact reconnect grace window, and whether the VOD splits, are **unverified**: measure in Phase 1.

**Reconnect.** ffmpeg does not retry a dropped RTMP output on its own. Wrap it with `Restart=always`, `RestartSec=5`, plus an outer backoff so a Twitch-side outage does not hammer the ingest. The `-reconnect*` flags apply to HTTP inputs, not RTMP outputs.

**2026-09-15 update: resolution decision changed.** The broadcast canvas is now native
1920x1080, and the Twitch target is **1080p30 at 6000 kbps CBR, 2 s keyframes**, not the
720p30/3000 kbps chosen above. Twitch's own encoder recommendations, verified against
[Broadcasting Guidelines](https://help.twitch.tv/s/article/broadcasting-guidelines?language=en_US)
(the page is still JS-rendered and did not fetch cleanly, so this is corroborated across
several secondary sources that quote it consistently, same caveat as above): **1080p30 at
4500 kbps** and **1080p60 at 6000 kbps**, both CBR with a 2 s keyframe interval. 6000 kbps
is Twitch's own figure for the 1080p*60* tier, not 1080p30 — running it at 30 fps sits
above Twitch's stated 1080p30 recommendation, not within it; flagged here rather than
silently reconciled. `infra/units/flycast.service`, `infra/units/xvfb.service`, and
`infra/config/chromium-flags` were updated to 1920x1080 / 6000k accordingly
(`infra/docs/p0-measurements.md` and `docs/design/infra.md` section 1 carry matching
dated notes). This paragraph and the reference command above are left as written for the
record of the original 720p decision, not rewritten.

**Two demos need two channels.** A stream key is per channel and only one encoder connection is accepted at a time; pushing the same key from two encoders drops the second ([stream key FAQ](https://stream-rise.com/blog/twitch-stream-key-faq)). "Simulcasting" in Twitch's vocabulary means the same broadcast to several platforms, not two broadcasts on one channel ([Simulcasting Guidelines](https://help.twitch.tv/s/article/simulcasting-guidelines?language=en_US)). So: **two accounts, two channels, two keys.** Whether one person may hold two broadcasting accounts for two projects is **unverified**; check the Twitch ToS rather than assuming.

## 4. VM design on Proxmox

**VM, not LXC.** An unprivileged LXC would need nesting and would still fight the Chromium sandbox, and the usual escape hatch (`--no-sandbox`) is exactly the wrong trade for a process running SwiftShader 24/7. A VM also gives clean `qm` snapshots before adapter upgrades and isolates a runaway encoder from the the neighbouring production service and rack-panel containers. Cost is ~500 MB RAM overhead per VM, which is affordable.

**Identity and placement.** The low id blocks on this host are already taken by other services and by another of the operator's projects, so take the next free pair for **`fly-pokemon`** and **`fly-platformer`**, and claim them in the host's $AGENT_CLAIM_LOG per the operator's rule. Debian 13 genericcloud qcow2 imported to `/var/lib/vz/import/` and driven by cloud-init, matching the pattern in the operator's own VM-build notes. UEFI q35, virtio-scsi, `onboot=1`, DHCP on vmbr0, `cpu: host`, 8 vCPU / 8 GB / 32 GB rootfs on local-zfs, plus a separate 16 GB disk at `/srv/fly/saves` so checkpoints survive a rootfs rebuild. Stage the FAFB artifacts (`positions.binz`, `classes.binz`, `viewer-edges.binz`, `circuit-roles.json`) and the ROM out of git, same posture as ROMs today; check their real sizes before fixing disk size.

**systemd units**, all `After=network-online.target`, app running as an unprivileged `fly` user:

1. `fly-app.service`: `npm run preview -- --host 127.0.0.1` in `/srv/fly/app`, with `POKEMON_SAVE_DIR=/srv/fly/saves` and `POKEMON_ROM=/srv/fly/rom/<file>.gb`, `Restart=always`. Bind 127.0.0.1 only; nothing here is an internet service.
2. `fly-display.service`: `Xvfb :99 -screen 0 1280x720x24 -nolisten tcp`, `Restart=always`.
3. `fly-browser.service`: Chromium with the section 2 flags, `DISPLAY=:99`, after 1 and 2, `Restart=always RestartSec=5`.
4. `fly-stream.service`: the ffmpeg command, `Requires=fly-display.service`, credential-loaded key, `Restart=always`.
5. `fly-stream-restart.timer`: `OnUnitActiveSec=23h` firing `systemctl restart fly-stream.service`. The 48 h guard.
6. `fly-watchdog.service` + `.timer`, every 60 s: if the newest file in `/srv/fly/saves` is older than 60 s the sim is stalled or the lease is lost, so restart `fly-browser`; if ffmpeg's `-progress` frame counter is not advancing, restart `fly-stream`. Better still, scrape the HUD over CDP (`--remote-debugging-port=9222` on 127.0.0.1) and alert on `SESSION BUSY`, `UPLOAD FAILED` or `SAVE INCOMPATIBLE`, all of which the app already reports. Use a restart counter and exponential backoff so a genuinely broken build does not restart-loop for days.

**Logs.** Everything to the journal, capped: `/etc/systemd/journald.conf.d/` with `SystemMaxUse=512M` and `MaxRetentionSec=14day`. ffmpeg at `-loglevel warning` only, or it writes a line per second forever. Chromium stderr is noisy under SwiftShader; consider `StandardError=null` on `fly-browser` once the noise is understood, not before.

**Checkpoint durability.** Local first: `/srv/fly/saves` on its own virtual disk, already fsync plus atomic rename on every 5 s commit. Offsite nightly, mirroring the existing the neighbouring production service cron (the host to the backup host at `<backup-user>@<backup-path>/<other-service>`): rsync to `<backup-user>@<backup-host>:<backup-path>/fly-pokemon/` and `.../fly-platformer/`. `docs/server-sessions.md` says to back up with the server stopped because `manifest.json` is the commit point, but we will not stop nightly. Instead copy in manifest-consistent order: read `manifest.json` and note `latest`/`previous`; copy those `<generation>.checkpoint` files; copy `manifest.json` last. A concurrent commit can only add a newer generation, so the snapshot is a valid older point in time. Also keep a weekly `qm snapshot`, and once a week do a genuine stop-copy-start archive to prove the restore path.

**"Is it actually live?"** Two independent layers, because each catches a different failure. Local: ffmpeg `-progress` frames advancing plus checkpoint mtime freshness, which catches a frozen sim still streaming a static picture (invisible to Twitch). Remote: `GET https://api.twitch.tv/helix/streams?user_login=<channel>` with an app access token from the client-credentials flow; it accepts an app or user token and needs no scope ([Helix reference](https://dev.twitch.tv/docs/api/reference/)). An empty `data` array means offline. Poll every 2 to 5 minutes from one place only, and alert on two consecutive offline polls. Store client id and secret like the stream key. The empty-array semantics are **unverified** from the docs page, which did not render its parameter table for automated fetch: confirm against a live and an offline channel in Phase 1.

## 5. Twitch chat integration (future hook)

Two mechanisms. IRC: `irc.chat.twitch.tv` port 6697 TLS (6667 plaintext); read-only anonymous login works with nick `justinfan<digits>` and no token, while sending or reading as a named bot needs a user token with `chat:read`/`chat:edit` sent as `PASS oauth:<token>` ([IRC concepts](https://dev.twitch.tv/docs/chat/irc/), [chatbot guide](https://dev.twitch.tv/docs/chat/chatbot-guide/)). EventSub over WebSocket: connect to `wss://eventsub.wss.twitch.tv/ws`, take the `session_welcome` session id, subscribe to `channel.chat.message` ([handling websocket events](https://dev.twitch.tv/docs/eventsub/handling-websocket-events), [subscription types](https://dev.twitch.tv/docs/eventsub/eventsub-subscription-types/)). EventSub is the forward-looking path.

Plumbing sketch: a small Node sidecar in the VM holds the chat connection and POSTs normalised events to a new local endpoint on the session server, which forwards them into the worker over the existing `HostMessage` channel. Use it for overlays first (a chat ticker in the page). Feeding chat into rewards changes the science claim and the checkpoint compatibility contract, so it needs its own design note and a `REWARD_ADAPTER` bump. Start read-only.

## 6. Demo 2: the platformer adapter

### Super Mario Land (GB, 1989) RAM map

Source: https://datacrystal.tcrf.net/wiki/Super_Mario_Land/RAM_map (fetched 2026-09-15, quoted as given; not independently verified against a ROM). Prefer WRAM/HRAM, which hold real values:

| Address | Size | Datacrystal description |
| --- | --- | --- |
| `0xC0A0` | 3 | Score (binary-coded decimal) |
| `0xC0A3` | 1 | Lives earned or lost |
| `0xC0A4` | 1 | ? (0x39 = Game Over) |
| `0xC0A9` | 1 | Superball duration |
| `0xC0AC` | 1 | Mario death jump timer |
| `0xC0D3` | 1 | Mario Starman timer |
| `0xC201` | 1 | ? Mario's Y position relative to the screen |
| `0xC202` | 1 | ? Mario's X position relative to the screen |
| `0xC203` | 1 | Mario's pose |
| `0xC207` | 1 | Jump state indicator |
| `0xC208` | 1 | Mario's Y speed |
| `0xC20A` | 1 | Mario is on the ground flag |
| `0xC20C` | 1 | ? Absolute value of Mario's X speed |
| `0xC20D` | 1 | Direction Mario faces while walking |
| `0xD1XY` | 160 | Object table (Y=0 type, 1 HP, 2 Y pos, 3 X pos, 6 pose, 8 anim timer) |
| `0xDA00` | 3 | Timer details |
| `0xDA15` | 1 | Lives (binary-coded decimal) |
| `0xFF99` | 1 | Powerup status |
| `0xFF9A` | 1 | Hard mode flag |
| `0xFFA6` | 1 | Powerup status timer |
| `0xFFB3` | 1 | ? (0x39 = Game Over) |
| `0xFFB5` | 1 | Does Mario have the Superball |
| `0xFFFA` | 1 | Coins (binary-coded decimal) |

The same page lists VRAM addresses, which are on-screen HUD **tile indices**, not values: `0x9806` lives displayed (copy of `0xDA15`), `0x9820` score displayed (copy of `0xC0A0`), `0x9829`/`0x982A` coin tens/ones, `0x982C` current world, `0x982E` current stage, `0x9831`-`0x9833` timer digits.

Two cautions. First, a VRAM read returns a tile index, so a digit needs an empirically found tile-base offset that can differ between HUD fonts; use WRAM/HRAM wherever the value exists there. Second, world and stage appear in this map **only** in VRAM, and CPU-bus reads of VRAM are blocked during LCD mode 3 on real hardware. Whether binjgb's `_emulator_read_mem` enforces that is **unverified**: if it does, sample only during VBlank, or better, find the WRAM source of world/stage with a RAM search (bgb or mGBA watchpoints on a level transition). Global x scroll for level progress is not in this map at all and also needs a search.

### Reward rules sketch (needs review before implementation)

Mirror `src/reward/catalog.ts`: sparse, gated, novelty-limited. `stage` when world/stage first increases (value 3); `checkpoint` when furthest-right screen x in this stage beats the previous best by N pixels (0.05, capped per stage); `coin` on `0xFFFA` BCD increase (0.05); `score` on `0xC0A0` BCD increase, delta-scaled and capped (0.1); `powerup` on `0xFF99`/`0xFFB5` improving, first per stage (0.5); `death` on `0xDA15` decreasing or `0x39` at `0xC0A4`/`0xFFB3`.

Open questions: whether to use negative reward at all (the existing catalog is strictly non-negative and the rule is eligibility-trace based, so a sign change is a real semantic change); how to stop the fly farming the first screen's coins; whether a timer penalty is needed for standing still. Progress-only-forward with a per-stage cap is the safest start. Also, a platformer punishes latency far harder than Pokemon does: at ~20 effective fps, precise jumps may be impossible. That is a legitimate thing to stream, but set expectations.

### Alternatives, with what actually exists

- **Kirby's Dream Land** ([RAM map](https://datacrystal.tcrf.net/wiki/Kirby's_Dream_Land:RAM_map), marked a stub, fetched 2026-09-15): `0xD05C` X on screen, `0xD05D` Y on screen, `0xD054`/`0xD056` sub-pixels, `0xD086` current health, `0xD089` lives, `0xD06F`-`0xD073` displayed score digits, `0xD08B`-`0xD08D` score/10 capped at `0x01869F`, `0xD074`/`0xD075` X speed, `0xD078`/`0xD079` Y speed, `0xD051` actual scroll X (resets each room). **Best-documented option for a progress signal**, because `0xD051` is a real scroll position and health/lives/score are all in WRAM with no VRAM decoding. Also a gentler game.
- **Wario Land: Super Mario Land 3** ([map exists](https://datacrystal.tcrf.net/wiki/Wario_Land:_Super_Mario_Land_3/RAM_map)) and **Wario Land 3** ([map exists](https://datacrystal.tcrf.net/wiki/Wario_Land_3:RAM_map)): contents not fetched, **unverified**.
- **Donkey Kong (GB, 1994)**: no datacrystal RAM map found. Only [Donkey Kong Land](https://datacrystal.tcrf.net/wiki/Donkey_Kong_Land/RAM_map) and [Donkey Kong Land III](https://datacrystal.tcrf.net/wiki/Donkey_Kong_Land_III/RAM_map) have maps, contents not fetched. Deprioritise DK '94.
- Index for anything else: https://datacrystal.tcrf.net/wiki/Category:RAM_maps

Recommendation: **Super Mario Land as the headline (recognisable), Kirby's Dream Land as the fallback** if the SML world/stage and scroll search stalls. Decide after a two-hour RAM-search spike, not from the wiki alone. ROM sourcing and legality stay the user's call, same posture as the existing Pokemon ROM: local only, gitignored, hash-pinned into the compatibility string.

## 7. Phased action plan

### Phase 0: prove headless capture locally

Target the WSL box or a throwaway the host VM. Not the neighbouring production container, not any live data.

1. Install Xvfb, Chromium, ffmpeg; start Xvfb :99 at 1280x720x24.
2. `npm run build && npm run preview` against a throwaway `POKEMON_SAVE_DIR`, never the real `local/saves`.
3. Launch Chromium headful on :99 with the section 2 flags including `--enable-unsafe-swiftshader`. Screenshot with `xwd` or `import` to confirm the connectome actually renders under SwiftShader.
4. Over CDP on 127.0.0.1:9222, log `document.visibilityState`, rAF delta and HUD emulator fps for 60 minutes. Confirm no throttling and no fps decay.
5. Run the section 3 ffmpeg command to a local file instead of Twitch; measure per-process CPU with `pidstat` and record totals.
6. Repeat with `-preset ultrafast` and compare sim fps.

Risks: SwiftShader too slow for 139k points; sim fps collapses once the encoder competes; Chromium leaks memory over hours; `/dev/shm` too small.

Go/no-go: **go** if 4 hours unattended holds emulator fps within 20 percent of the no-capture baseline, the output is clean CFR 30 fps 720p H.264+AAC, total CPU is under 6 cores, and RSS is flat.

### Phase 1: Pokemon VM live on Twitch

1. Decide the account plan, generate key #1, store as `pass twitch/<channel>-key`.
2. Claim the first demo VM in the host's `$AGENT_CLAIM_LOG`; build the Debian 13 cloud-init VM, 8c/8G/32G plus a 16G saves disk.
3. Stage app, ROM and dataset artifacts. Migrate the current checkpoint from the WSL box per `docs/server-sessions.md` (server stopped, whole directory copied), or start fresh and accept the reset.
4. Install the four services and two timers from section 4.
5. Point ffmpeg at the real RTMPS URL with the credential-loaded key. Go live.
6. Verify the 23 h ffmpeg restart leaves the sim running and produces a new VOD. Verify the Helix poller flips offline when ffmpeg stops, and back when it returns.
7. Wire the nightly rsync to the backup host and prove one restore into a scratch VM.

Risks: the key leaking into a log or commit (grep the repo and the journal for the key prefix before calling it done); the 23 h restart being treated as a brand-new stream that resets viewer state; the watchdog fighting the lease during a browser restart; ZFS write amplification from a 5 s fsync cadence on local-zfs.

Go/no-go: **go to Phase 2** after 7 unattended days with no manual intervention, at least 6 clean daily restarts, a verified restore, and the offline alert firing correctly in a deliberate test.

### Phase 2: platformer adapter and second VM

1. Two-hour RAM-search spike in bgb or mGBA: find WRAM world, stage and global scroll for Super Mario Land. If not found, switch to Kirby's Dream Land and use `0xD051`.
2. Write `src/reward/<game>.ts` against the existing `MemoryReader` interface, plus a game catalog and a new `REWARD_ADAPTER` string.
3. Extend `src/runtime/compatibility.ts` so the compatibility identity carries the game id and the new ROM SHA-256, then confirm a Pokemon checkpoint is rejected by the platformer build and vice versa. The design already does `SAVE INCOMPATIBLE` on mismatch, so this should be a test, not new code.
4. Confirm the motor decoder is usable for a platformer at all. It is fixed after startup calibration and tuned to Pokemon's menu-driven pacing; a platformer needs sustained holds, so expect a button hold/cooldown retune as its own reviewed change.
5. Unit tests for the new reward rules with synthetic memory fixtures, in the existing test style.
6. Build the second demo VM on the same recipe, key #2, second channel.

Risks: world/stage reachable only through VRAM and binjgb blocking mode-3 reads; a reachable reward-farming loop; the decoder simply cannot clear World 1-1 at 20 fps, making the stream a wall of deaths; two VMs oversubscribing the host.

Go/no-go: **go** if the adapter fires the expected reward kinds on a scripted playthrough fixture, no farming loop is reachable in a 1 hour soak, and the host load average with both VMs live stays under 0.7x its core count.

### Phase 3: overlays and chat

1. Add a stream overlay layer inside the page (uptime, total reward, stage or badge count, last reward, and a "this is a simulation, not a real fly" disclaimer) so x11grab needs no change.
2. Read-only chat sidecar, IRC anonymous first and EventSub later, rendering a chat ticker in the overlay. No reward coupling.
3. Twitch panels and channel metadata pointing at the FlyWire and Shiu et al. references already in the README.
4. Only then consider chat-influenced rewards or viewer votes, with a written design note first.

Risks: overlay repaint cost eating the sim's core; moderation exposure once chat text is rendered on stream, which needs a blocklist and a mod plan first.

Go/no-go: **go** if overlay plus chat adds under 0.3 core and a moderation policy exists in writing.

### Phase 4 (optional, later): drop the browser

Revisit option (c). Port the worker to Node, keep binjgb WASM, replace the three.js map with `headless-gl` or a software point rasteriser, pipe rawvideo straight to ffmpeg. This removes Chromium, Xvfb, SwiftShader and the lease protocol and would likely halve CPU per demo. Only worth it if Phases 1 to 3 show the browser stack is the bottleneck or the instability.

## 8. Open items to verify before building

- The host `lscpu`/`free -g`/current load, and free headroom after the neighbouring production container, the metrics container, another container on the host, another container on the host, another container on the host, another guest on the host.
- Whether an Xvfb-mapped Chromium window really reports `visible` and escapes all throttling over hours. Measure, do not assume.
- SwiftShader throughput on the 139k-point connectome at 720p.
- Twitch ToS position on one person operating two broadcasting accounts.
- Twitch reconnect grace window, and whether a 23 h restart splits the VOD.
- Whether binjgb's `_emulator_read_mem` gates VRAM reads by LCD mode.
- The exact Helix `Get Streams` parameter table (the docs page did not render for automated fetch; confirm `user_login` semantics by hand).
- Super Mario Land WRAM addresses for world, stage and global scroll.
