**DESPLIO**

Virtual Display Streaming Platform

Product Requirements & Architecture Document  —  v1.0

| Project | Desplio |
| :---- | :---- |
| Author | Eleyoungtech Industries |
| Version | 1.0 — Initial Architecture |
| Status | In Planning |
| Target (v1) | Linux host • All-platform clients |
| Date | May 2026 |

# **1\. Executive Summary**

Desplio is a production-grade virtual display streaming daemon that turns any networked device — phone, tablet, smart TV, laptop, or desktop — into a fully functional external monitor with bidirectional input. No proprietary hardware required. No USB dongle. No Windows dependency.

The core problem: Spacedesk, the closest existing solution, only supports Windows as the host. There is no serious competitor that works on Linux. For the global developer and power-user market that runs Linux as their primary OS, this gap is felt daily. Desplio fills it with a zero-compromise implementation that is architecturally superior to Spacedesk from day one.

Desplio is built on two binaries: a Rust host daemon that manages virtual display creation, GPU-accelerated frame capture, hardware encoding, and input injection at the kernel level; and a Tauri client shell that provides the viewer UI on any desktop OS. Mobile and TV clients are served by a thin Capacitor app using the same WebRTC signalling layer.

## **1.1 Why This Wins**

| Feature | Desplio | Spacedesk | VirtualHere / DisplayLink |
| :---- | :---- | :---- | :---- |
| Linux host | ✓ Native, v1 | ✗ Windows only | ✗ Hardware dongle |
| macOS / Windows host | v2 roadmap | ✓ Windows | ✓ Dongle only |
| Phone client | ✓ iOS \+ Android | ✓ Both | ✗ None |
| TV / browser client | ✓ WebRTC | ✗ None | ✗ None |
| Another laptop client | ✓ Tauri \+ Browser | ✓ Windows only | ✗ None |
| Hardware encoder | ✓ VAAPI / NVENC | Software only | N/A |
| Open protocol | ✓ WebRTC \+ JSON | ✗ Proprietary | ✗ Proprietary |
| Touch → pointer | ✓ Full MT | ✓ | ✗ |
| Multi-monitor | v2 roadmap | ✓ | ✗ |

# **2\. Problem Statement**

## **2.1 The Linux Gap**

Linux runs on an estimated 3–4% of all desktop machines globally — but a disproportionately large share of developer and power-user workstations. For these users, wanting a second monitor means buying another physical display. Their phones, tablets, and spare laptops cannot be used as displays because no mature software solution exists for the Linux host side.

Existing workarounds (xrandr \+ VNC, Sunshine \+ Moonlight for gaming) require significant configuration expertise, produce laggy results, and do not solve the use case cleanly. The market is completely unserved.

## **2.2 Spacedesk’s Ceiling**

Spacedesk is the benchmark, and it has hard limitations:

* Windows-host-only architecture baked into their driver model (IddCx, Windows Display Driver Model)

* No browser or Smart TV client

* Software-only encoding — high CPU cost, poor performance on older machines

* Proprietary binary protocol with no public spec

* Paid tier required for multi-monitor on newer versions

# **3\. Product Vision & Scope**

## **3.1 North Star**

"Any screen within reach becomes a usable monitor. No cables, no dongles, no fuss. And it actually works on Linux."

Desplio’s long-term vision is to be the universal virtual display layer — the software that makes every screen in your environment participatory in your desktop. Phone on your desk? Second monitor. Old laptop in the corner? Third monitor. TV across the room? Presentation display.

## **3.2 MVP Scope (v1.0)**

| SCOPE | v1 ships a Linux host only. Client apps target Android first, with a web client (browser) as a zero-install fallback that works everywhere. |
| :---- | :---- |

* Linux host daemon (Wayland-primary, X11 supported)

* One virtual display at user-defined resolution (up to 4K)

* H.264 hardware-encoded stream via VAAPI (Intel/AMD) with NVENC fallback for Nvidia

* WebRTC transport — not WebSocket, not TCP. WebRTC from day one for latency and NAT resilience

* Android client via Capacitor \+ React (hardware H.264 decode, WebRTC)

* Web client (browser tab) as universal fallback — WebCodecs \+ WebRTC, works on any OS

* Touch-to-pointer input injection on the host via uinput

* Keyboard pass-through (soft keyboard on phone, physical keyboard on laptop client)

* mDNS auto-discovery on local network — no IP address needed

* PIN-based pairing for security

## **3.3 v2 Roadmap (Post-Launch)**

* macOS host (CoreGraphics virtual display API)

* Windows host (IddCx virtual display driver)

* iOS client

* Tauri desktop client (Windows \+ macOS \+ Linux) for laptop-to-laptop

* Smart TV / Android TV client

* Multi-monitor support (multiple virtual displays)

* Stylus / Apple Pencil input pass-through

* USB tethering transport (lower latency than Wi-Fi)

* Adaptive bitrate based on network conditions

* Display mirroring mode (mirror instead of extend)

* Desplio cloud relay for WAN use (outside LAN)

# **4\. System Architecture**

## **4.1 Component Map**

Desplio has three logical layers: the Host Daemon, the Transport Layer, and the Client Shell. These are separated by clean protocol boundaries so each can evolve independently.

| Layer | Component | Technology | Responsibility |
| :---- | :---- | :---- | :---- |
| Host Daemon | Virtual Display Manager | evdi kernel module \+ DRM | Create and manage virtual display devices in the OS |
| Host Daemon | Frame Capturer | KMS/DRM direct, PipeWire fallback | Grab raw frames from the virtual framebuffer at vsync |
| Host Daemon | Encoder Pipeline | libavcodec (ffmpeg), VAAPI, NVENC | Hardware-encode frames to H.264/H.265 NAL units |
| Host Daemon | WebRTC Engine | libdatachannel (Rust bindings) | Peer negotiation, ICE, DTLS, RTP stream |
| Host Daemon | Input Injector | uinput kernel module, libevdev | Inject pointer, key, and touch events from client |
| Host Daemon | Signalling Server | Axum (Rust), WebSocket | SDP offer/answer relay, discovery, pairing |
| Transport | WebRTC RTP | SRTP over UDP | Low-latency encrypted video stream to client |
| Transport | Data Channel | SCTP over DTLS | Input events, control messages, latency probes |
| Client Shell | Decoder | WebCodecs API / platform H.264 | Decode RTP stream, present frames to canvas |
| Client Shell | Renderer | WebGL canvas (OffscreenCanvas) | Zero-copy frame display at display refresh rate |
| Client Shell | Input Capture | Pointer Events API, KeyboardEvent | Capture touch/mouse/keyboard, send over data channel |
| Client Shell | Discovery UI | mDNS / manual IP | Find hosts on LAN, manage pairing |

## **4.2 Data Flow**

The end-to-end frame pipeline on the host side runs as follows:

1. The Linux kernel DRM subsystem writes compositor output to the evdi virtual framebuffer at every vsync interval (target 60Hz).

2. The Frame Capturer receives a DMA-BUF handle from the kernel — zero-copy, no memcpy from GPU to CPU.

3. The DMA-BUF handle is passed directly to the VAAPI/NVENC encoder. The frame never touches CPU RAM. GPU-in, encoded-out.

4. The encoder emits H.264 NAL units (Annex B format). These are passed to the WebRTC engine as raw RTP payloads.

5. libdatachannel packetises the RTP, applies SRTP encryption, and sends over UDP to the connected client peer.

6. The client WebRTC stack reassembles RTP packets, passes encoded frames to WebCodecs VideoDecoder.

7. WebCodecs returns decoded VideoFrame objects, which are transferred to an OffscreenCanvas and composited to the display.

8. Input events (pointer, keyboard, touch) captured on the client are serialised to JSON and sent back via the WebRTC Data Channel (SCTP, ordered, low-latency).

9. The Input Injector receives these events, validates and coordinate-maps them, and writes to /dev/uinput — appearing to the kernel as hardware input.

| LATENCY TARGET | End-to-end frame latency target on local Wi-Fi: \< 40ms. Breakdown: capture 2ms \+ encode 5ms \+ RTP packetise 1ms \+ network 10–15ms \+ decode 5ms \+ render 2ms \= \~30ms budget, 10ms headroom. |
| :---- | :---- |

# **5\. Host Daemon — Deep Specification**

## **5.1 Virtual Display Driver**

The virtual display must be indistinguishable from a real GPU output at the OS level. This is non-negotiable — it means apps must be able to drag windows onto it, the compositor must enumerate it as a real monitor, and system display settings must work with it.

### **5.1.1 Primary Path: evdi**

evdi (Extensible Virtual Display Interface) is a Linux kernel module maintained by DisplayLink. It creates a fully functional DRM device node. The compositor sees it as a DRM-KMS output and the OS assigns it a CRTC, encoder, and connector in the DRM graph.

* Load the evdi kernel module at daemon startup: evdi\_open(EVDI\_INVALID\_HANDLE) → returns handle to the new virtual device

* Register display EDID: set resolution, refresh rate, colour depth. The OS respects the EDID entirely.

* The daemon enters a DRM update loop: evdi\_handle\_events() to receive framebuffer update notifications

* Updates come as DRM\_EVDI\_GRABPIX events with a DMA-BUF fd attached — zero-copy path to the encoder

* On disconnect: evdi\_disconnect(), evdi\_close() — the virtual monitor disappears from the compositor

### **5.1.2 Wayland Fallback: wlr-virtual-output**

On Wayland compositors that support wlroots protocols (Sway, Hyprland, River, nwl), we can use the zwlr\_virtual\_output\_manager\_v1 protocol to create a virtual output without a kernel module. This requires no root access and is safer for containerised environments.

* Negotiate virtual output via Wayland socket (no evdi needed)

* Use PipeWire xdg-desktop-portal to capture the virtual output stream

* Slightly higher latency than DMA-BUF path — acceptable for v1 on compositors that don’t support evdi

### **5.1.3 X11 Fallback: xrandr dummy**

For X11 environments, use xf86-video-dummy driver with a synthetic EDID injected via xrandr \--addmode. Less featureful than evdi but universally compatible. The capture path uses XShm (shared memory) to grab frames from the X framebuffer.

## **5.2 Frame Capture Pipeline**

Frame capture is the most latency-sensitive stage. We implement three backends selectable at runtime based on environment:

| Backend | Path | Latency | Requires | Use When |
| :---- | :---- | :---- | :---- | :---- |
| DRM/KMS direct | DMA-BUF fd from evdi update callback | \~1–2ms | evdi module, root or DRM group | Primary path on bare metal Linux |
| PipeWire | pw\_stream with SPA\_VIDEO\_FORMAT\_BGRx | \~3–5ms | PipeWire 0.3+, portal | Wayland without wlr protocols, containerised |
| XShm | XShmGetImage() on X11 dummy display | \~5–8ms | X11, Xext | X11 fallback, lowest common denominator |

The capture loop runs on a dedicated OS thread pinned to a CPU core with SCHED\_FIFO priority to prevent frame drops due to scheduler jitter. Target: capture-to-encoder handoff in under 2ms.

## **5.3 Encoder Pipeline**

### **5.3.1 Hardware Encoder Selection**

The daemon probes for available hardware encoders at startup in this priority order:

10. VAAPI H.265 (Intel Gen9+, AMD RDNA2+) — best quality/bitrate ratio

11. VAAPI H.264 (Intel HD 4000+, most AMD) — widest client compatibility

12. NVENC H.264 (any Nvidia Maxwell+) — fastest encoder, highest quality

13. V4L2 M2M (ARM SoCs, Raspberry Pi) — for embedded Linux hosts

14. Software x264 (libavcodec) — absolute last resort, high CPU

Client H.265 support is detected during WebRTC capability negotiation (SDP). If the client reports no H.265 decoder, we fall back to H.264 regardless of encoder availability.

### **5.3.2 Encoder Configuration**

Low-latency live streaming requires specific encoder tuning. These are not defaults — they must be set explicitly:

* Preset: ultrafast (x264 software) / low\_latency\_high (NVENC) / CBR (VAAPI)

* Tune: zerolatency — disables lookahead, B-frames, and scene-cut analysis

* GOP structure: IDR every 60 frames (1 second at 60Hz). Closed GOP. No B-frames.

* Rate control: CBR at 8–20 Mbps depending on resolution. No VBR — VBR causes decoder stalls.

* Slice mode: one slice per frame, intra-refresh enabled as IDR alternative

* Colour space: YUV 4:2:0, BT.709, full range

* Threading: encoder runs on dedicated thread pool, never blocks capture or network

### **5.3.3 Adaptive Quality**

The encoder monitors RTT and packet loss from WebRTC RTCP feedback. If RTT exceeds 80ms or packet loss exceeds 2%, the daemon steps down bitrate by 20% and requests an IDR keyframe. If conditions improve for 5 consecutive seconds, bitrate steps back up. This prevents the frozen-frame experience that kills usability.

## **5.4 WebRTC Engine**

We use libdatachannel (C++ library with Rust FFI bindings via datachannel-rs) as the WebRTC implementation. This avoids the massive footprint of libwebrtc (Google’s full implementation) while providing everything we need: ICE, DTLS, SRTP, RTP, and Data Channels.

### **5.4.1 Signalling**

WebRTC requires a signalling channel to exchange SDP offers/answers and ICE candidates before the peer connection is established. Desplio runs a lightweight Axum WebSocket signalling server as part of the daemon. The flow:

15. Client connects to ws://host-ip:7472/signal

16. Client sends { type: 'hello', pin: '1234', capabilities: { codecs: \['h265','h264'\], maxResolution: '3840x2160' } }

17. Host validates PIN, sends SDP offer with video track (H.264 or H.265 based on client caps)

18. Client sends SDP answer

19. Both sides exchange ICE candidates (trickle ICE)

20. ICE connectivity check completes, DTLS handshake, SRTP keys derived

21. Video RTP and Data Channel go live. Signalling WebSocket remains open for control messages.

### **5.4.2 ICE Configuration**

On a local network, mDNS-resolved host candidates are sufficient. For future WAN support, we integrate a TURN server (coturn, self-hosted) to relay traffic through NAT. STUN via Google’s public servers for v1.

* ICE candidates: host (direct LAN), srflx (STUN-reflexive for future WAN), relay (TURN for future WAN)

* ICE consent freshness: 30-second STUN binding refresh to detect disconnects

* DTLS fingerprint pinning: prevents MITM on local network

## **5.5 Input Injection**

This is the feature that makes Desplio a real second monitor rather than a dumb display. Input from the client must arrive on the host as kernel-level events, indistinguishable from a physical device.

### **5.5.1 uinput Device Setup**

At daemon startup, we create three uinput virtual devices:

* Virtual mouse: reports ABS\_X, ABS\_Y in the coordinate space of the virtual display. Relative to the virtual display origin in the X/Wayland compositor input space.

* Virtual keyboard: full 105-key layout, reports KEY\_\* codes, supports modifier combinations

* Virtual touch device: reports ABS\_MT\_POSITION\_X/Y, ABS\_MT\_SLOT, ABS\_MT\_TRACKING\_ID for proper multi-touch. Also reports BTN\_TOUCH and ABS\_PRESSURE.

These devices appear in /proc/bus/input/devices and are treated by the compositor as real hardware. Wayland compositors route events from these devices to whichever window has focus on the virtual display.

### **5.5.2 Coordinate Mapping**

Client sends pointer events in client-screen-normalised coordinates (0.0 to 1.0 on each axis). The daemon maps these to the virtual display’s resolution and injects as ABS events. This means the mapping is resolution-independent and works when the client and virtual display have different aspect ratios (with letterboxing/pillarboxing on the client side to preserve fidelity).

### **5.5.3 Input Event Format (Data Channel)**

Events are sent as compact binary-encoded JSON over the WebRTC Data Channel (SCTP, ordered delivery). Format:

{ "t": "ptr", "x": 0.512, "y": 0.301, "b": 0, "ts": 1716123456789 }

{ "t": "key", "code": "KeyA", "mod": \["Ctrl", "Shift"\], "down": true }

{ "t": "touch", "id": 0, "x": 0.4, "y": 0.6, "p": 0.8, "phase": "move" }

{ "t": "scroll", "dx": 0, "dy": \-3 }

The ‘ts’ timestamp is used by the host to detect and discard stale events if the data channel buffers during a network hiccup. Events older than 200ms are dropped rather than injected out of order.

# **6\. Client Architecture**

## **6.1 Client Philosophy**

The client is intentionally thin. It has one job: receive an H.264/H.265 RTP stream, render it at full frame rate, and send input events back. All intelligence lives in the host daemon. This means we can support new client platforms by writing a thin shell — not porting business logic.

| PRINCIPLE | The client must work in any modern browser with no install. The Capacitor app is a performance optimisation and UX improvement, not a requirement for functionality. |
| :---- | :---- |

## **6.2 Web Client (Universal Fallback)**

A single-page web app served by the host daemon at http://host-ip:7473/. Any device with a modern browser can use Desplio by navigating to this URL. No app store, no installation.

* WebRTC peer connection via browser RTCPeerConnection API

* Video decode via WebCodecs VideoDecoder API (Chrome 94+, Edge 94+, Safari 16.4+, Firefox 130+)

* Rendering via OffscreenCanvas \+ requestVideoFrameCallback for sub-frame-accurate display

* Input capture via Pointer Events API (handles mouse and touch uniformly), KeyboardEvent

* Discovery: user navigates to the URL shown in the host daemon tray app. mDNS resolution handled by the OS.

* Fullscreen via Fullscreen API, Screen Orientation API to lock landscape on phones

### **6.2.1 OffscreenCanvas Rendering**

We use OffscreenCanvas with a dedicated Worker to ensure frame rendering never blocks the main thread. The pipeline:

22. WebCodecs VideoDecoder outputs VideoFrame objects on the main thread

23. VideoFrame is transferred (zero-copy) to the OffscreenCanvas worker via postMessage with transfer

24. Worker calls ctx.drawImage(videoFrame) on the OffscreenCanvas at requestAnimationFrame timing

25. VideoFrame.close() immediately after draw to release GPU memory

This keeps frame delivery and rendering on separate threads, preventing jank from input handling or React re-renders blocking the video.

## **6.3 Android / iOS Client (Capacitor)**

The Capacitor app wraps the web client in a native shell with performance-critical paths replaced by native implementations where the browser falls short.

| Concern | Browser Fallback | Capacitor Native Plugin |
| :---- | :---- | :---- |
| H.264 Decode | WebCodecs (good, some overhead) | Native MediaCodec (Android) / VideoToolbox (iOS) via Capacitor plugin |
| Rendering | OffscreenCanvas WebGL | SurfaceView / AVSampleBufferDisplayLayer directly from native decoder |
| Wake Lock | Screen Wake Lock API (unreliable) | PowerManager.WakeLock (Android), UIApplication.idleTimerDisabled (iOS) |
| Keyboard | Virtual keyboard via Pointer Events | Native IME integration, physical keyboard pass-through |
| Network | Standard WebRTC | Same WebRTC, but OS network stack has priority QoS over browser tab |
| Background | Tab freezes in background | Native foreground service keeps connection alive |

The Capacitor plugin for native video decode is the one piece of genuinely complex platform code. It uses a Capacitor bridge to pass the WebRTC RTP stream into MediaCodec/VideoToolbox and renders directly to a native SurfaceView. This reduces decode latency by \~8ms compared to WebCodecs on mid-range Android devices.

## **6.4 Tauri Desktop Client (v2)**

For laptop-to-laptop use, we ship a Tauri app. The Tauri shell provides:

* System tray integration: persistent background connection, quick reconnect

* Display mirroring mode: a non-fullscreen window that can be resized, allowing the laptop to act as a floating display panel

* Hardware cursor: the client renders a native OS cursor rather than a soft cursor, eliminating the double-cursor problem

* Keyboard grab: Tauri can capture global keyboard shortcuts that browsers cannot (e.g., Ctrl+Alt+Del, media keys)

* USB tethering transport (v2.1): when connected via USB, use ADB reverse port forwarding to route the WebRTC stream over USB instead of Wi-Fi — drops latency to \~10ms

# **7\. Security Model**

## **7.1 Threat Model**

Desplio runs on a local network. The primary threats are:

* Unauthorized access: someone on the same network connecting to the host without permission

* Stream interception: passive sniffing of the display stream on the LAN

* Input injection: an attacker sending input events to the host

* Privilege escalation: the daemon running as root exposes attack surface

## **7.2 Mitigations**

| Threat | Mitigation |
| :---- | :---- |
| Unauthorized access | 6-digit PIN displayed on host, required on first connection. PIN rotates on every disconnect. Pairs are stored as DTLS fingerprint \+ client ID, not PIN. |
| Stream interception | WebRTC mandates SRTP. All video is encrypted. An attacker who intercepts UDP packets sees only ciphertext. |
| Input injection | Only paired clients (validated DTLS fingerprint) can send Data Channel messages. Input events from unpaired peers are dropped. |
| Privilege escalation | Daemon drops to a dedicated 'desplio' user after loading kernel modules. uinput device ownership set to that user. No capabilities retained except CAP\_NET\_BIND\_SERVICE. |
| Local network MITM | DTLS certificate fingerprint is displayed on host UI and client during pairing. User can verify out-of-band. |

# **8\. Full Technology Stack**

## **8.1 Host Daemon (Rust)**

| Crate / Library | Purpose | Notes |
| :---- | :---- | :---- |
| tokio | Async runtime | Multi-threaded, work-stealing. Foundation for all async I/O. |
| axum | HTTP \+ WebSocket signalling server | Tower middleware, TLS via rustls |
| datachannel-rs | WebRTC (libdatachannel Rust bindings) | ICE, DTLS, SRTP, RTP, Data Channels |
| evdi-sys | evdi kernel module bindings | Custom unsafe Rust bindings to evdi C API |
| drm-rs | Linux DRM/KMS interface | DMA-BUF import, CRTC management |
| pipewire-rs | PipeWire capture backend | Wayland fallback capture |
| ffmpeg-next | libavcodec encoder pipeline | VAAPI, NVENC, software encode |
| evdev | uinput virtual device creation | Input injection for pointer, keyboard, touch |
| mdns-sd | mDNS service advertisement | \_desplio.\_tcp.local. TXT records with host metadata |
| serde / serde\_json | Input event serialisation | Data channel message parsing |
| tracing \+ tracing-subscriber | Structured logging | JSON output for production, pretty for dev |
| clap | CLI argument parsing | Daemon configuration overrides |
| config-rs | Config file management | TOML config at \~/.config/desplio/config.toml |
| keyring | Secure PIN / pairing storage | libsecret on Linux, Keychain on macOS (v2) |

## **8.2 Client (Web / Capacitor)**

| Library | Purpose | Notes |
| :---- | :---- | :---- |
| React 19 | UI framework | Consistent with Flustro / ProspectAI stack |
| @capacitor/core | Native bridge | iOS \+ Android packaging |
| WebRTC (browser native) | P2P video \+ data | RTCPeerConnection, RTCDataChannel |
| WebCodecs API | H.264/H.265 decode | VideoDecoder, VideoFrame, OffscreenCanvas |
| Zustand | Client state | Connection state, settings, display config |
| Vite | Build tool | Fast HMR, tree-shaking |
| Tailwind CSS | Styling | Utility-first, consistent with existing apps |

# **9\. Repository & File Structure**

Monorepo layout with Cargo workspace for Rust and npm workspaces for client:

desplio/

  apps/

    daemon/          \# Rust: host daemon binary

      src/

        main.rs      \# entry point, signal handling, config

        display/     \# evdi, wlr-virtual-output, xrandr backends

        capture/     \# DRM/KMS, PipeWire, XShm backends

        encoder/     \# VAAPI, NVENC, software encoder abstraction

        webrtc/      \# libdatachannel wrapper, signalling, ICE

        input/       \# uinput device creation, event injection

        server/      \# Axum HTTP/WS: signalling, web client serve

        discovery/   \# mDNS advertisement

        config.rs    \# typed config with defaults

      Cargo.toml

    tray/            \# Tauri v2: system tray UI for host

      src-tauri/     \# Rust Tauri backend

      src/           \# React tray frontend

  packages/

    client-core/     \# Shared TS: WebRTC, WebCodecs, input capture

    ui/              \# Shared React component library

  apps/

    web-client/      \# SPA served by daemon

    mobile-client/   \# Capacitor iOS \+ Android

    desktop-client/  \# Tauri desktop client (v2)

  kernel/

    evdi-builder/    \# evdi DKMS build scripts

  scripts/

    install.sh       \# setup evdi, uinput, user permissions

    dev.sh           \# launch daemon in dev mode with logging

  docs/

  Cargo.toml         \# workspace root

  package.json       \# npm workspace root

# **10\. MVP Build Plan**

Milestone breakdown for v1.0. Each milestone is independently testable.

| Milestone | Deliverable | Estimated Effort | Key Risk |
| :---- | :---- | :---- | :---- |
| M0: Foundation | Cargo workspace, evdi kernel module loads, virtual monitor appears in xrandr/wlroots | 1 week | evdi DKMS build on target distro |
| M1: Frame Capture | DRM/KMS capture backend, raw frames dumped to PNG for verification | 1 week | DMA-BUF import format compatibility |
| M2: Encode | VAAPI H.264 encode pipeline, encoded stream written to file for quality check | 1 week | VAAPI driver availability in test env |
| M3: Signalling | Axum WS signalling server, SDP exchange in curl \+ browser test | 3 days | SDP negotiation edge cases |
| M4: WebRTC Video | Full RTP stream to browser tab, video plays (no input yet) | 1.5 weeks | libdatachannel RTP pacing, jitter buffer |
| M5: Input | uinput devices created, touch events from browser move cursor on host | 1 week | Coordinate mapping, Wayland input routing |
| M6: Discovery | mDNS advertisement, host found without IP in client UI | 3 days | mDNS multicast on all common Linux configs |
| M7: Security | PIN pairing, DTLS fingerprint verification, drop privileges | 4 days | Secure storage of pairing data |
| M8: Capacitor App | Android APK with native MediaCodec decode, ships to TestFlight/Play beta | 2 weeks | MediaCodec surface config with WebRTC |
| M9: Polish \+ Tray | Tauri tray app for host, connection status, settings, auto-start | 1 week | Tauri v2 IPC with daemon |
| M10: v1.0 Ship | Signed Linux packages (deb, rpm, AppImage), Android APK, web client hosted | 1 week | Packaging, install script, first-run UX |

| TOTAL | Estimated solo build time for v1.0: 10–12 weeks at consistent pace. Parallel work on M3–M5 is possible once M2 is proven. |
| :---- | :---- |

# **11\. Performance Targets & Measurement**

| Metric | v1 Target | Stretch Target | How Measured |
| :---- | :---- | :---- | :---- |
| End-to-end latency (Wi-Fi) | \< 40ms | \< 25ms | Timestamp comparison: frame captured vs frame displayed (camera on screen) |
| Frame rate (1080p) | 60fps | 60fps stable | Client VideoDecoder output frame rate counter |
| Frame rate (4K) | 30fps | 60fps (H.265 \+ NVENC) | Same |
| CPU usage (host, encode) | \< 5% on Core i5 8th gen | \< 2% | perf stat on encoder thread |
| Memory (daemon) | \< 80MB RSS | \< 50MB | valgrind massif or /proc/pid/status |
| Input latency | \< 15ms | \< 8ms | Hardware latency tester (LDAT or DIY photoresistor) |
| Connection setup time | \< 3 seconds | \< 1.5 seconds | Time from client tap to first frame |
| Battery drain (Android client) | \< 8% / hour | \< 5% / hour | Android Battery Historian |

# **12\. Open Questions & Decisions**

| Question | Options | Current Leaning | Decide By |
| :---- | :---- | :---- | :---- |
| Brand name | Desplio, Mirrex, Panex, Span, Voide, Extera | Desplio (clear, technical, memorable) | Before M8 |
| Pricing model | Free \+ Pro, one-time purchase, subscription | Free for 1 client, Pro for multi-monitor \+ TV \+ priority support | Before M10 |
| Host UI: tray vs full window | System tray only, full window, both | Tray for v1, settings window on demand | M9 |
| Android decode: WebCodecs vs native plugin | WebCodecs (simpler), MediaCodec plugin (faster) | Ship WebCodecs first, native plugin if benchmarks show \>10ms diff | M8 |
| TURN relay for WAN | Self-hosted coturn, Cloudflare TURN, skip for v1 | Skip for v1, LAN-only is fine | v2 planning |
| Linux package format for v1 | deb only, rpm \+ deb, AppImage, Flatpak | deb \+ AppImage, Flatpak in v1.1 | M10 |

# **13\. Non-Goals (v1)**

These are explicitly out of scope for v1 to keep the build focused:

* macOS or Windows host support

* Internet (WAN) connectivity — local network only

* Multi-monitor (more than one virtual display at a time)

* Display mirroring mode (extend only)

* Audio streaming (display only)

* USB tethering transport

* iOS client

* Stylus / pressure-sensitive input

* Remote desktop (reverse direction — controlling the client from the host)

* Any form of cloud service or account requirement

Desplio — PRD v1.0 — Eleyoungtech Industries — Confidential