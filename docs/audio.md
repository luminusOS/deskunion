# Audio streaming

DeskUnion can stream a client's audio to the server, so it plays from the server's speakers next to the shared mouse and keyboard. Streaming goes one way, client to server, and each end opts in separately: sending and receiving are toggled independently.

- The codec is Opus, sent over the same DTLS-encrypted UDP channel as input events.
- Capture and playback use [cpal](https://github.com/RustAudio/cpal): PipeWire loopback on Linux, WASAPI process loopback on Windows 10 build 20348 and later, and CoreAudio on macOS 14.6 and later. Older Windows builds fall back to endpoint loopback. Older macOS versions can capture a microphone but not system output.
- On Windows, process loopback excludes DeskUnion and ignores the output's mute and volume.
- A jitter buffer with clock-drift compensation absorbs network jitter and sender and receiver clocks that slowly diverge.
- The Audio page in the GTK frontend enables send and receive, picks capture and playback devices, sets the bitrate and jitter buffer size, and shows buffered audio, packet loss and signal level.
- The `audio` cargo feature is on by default. Build with `--no-default-features` to leave it out (see [building.md](building.md#conditional-compilation)).

## Teams calls on Windows

Process loopback receives only silence from the Teams desktop client (`ms-teams.exe`), a known Windows limitation ([microsoft/Windows-classic-samples#414](https://github.com/microsoft/Windows-classic-samples/issues/414)). Browsers, Zoom and other applications are captured normally.

DeskUnion therefore listens to the Windows audio notifications: a session was created or changed state, an output appeared or went away. While Teams plays on an output, DeskUnion captures that whole output instead. It goes back to process loopback 3 seconds after Teams falls silent. During that time the Windows volume and mute of the output apply, so a muted output is silent on the server, and sounds on other outputs are not sent.

`deskunion.log` in `%LOCALAPPDATA%\deskunion` records when this starts and stops. At the `debug` level (`DESKUNION_LOG_LEVEL=debug`) it also lists which applications have audio sessions. To capture one output all the time, choose it as the capture device on the Audio page.
