# qareel Linux browser host

On Linux, qareel drives a headless WPE WebKit browser (`qareel-browser`) that runs inside a rootless podman container. This directory holds its C sources, the build helper `native_webkit.py` and the `Containerfile`.

## Build the image locally

From the repository root:

```sh
podman build -f host-linux/Containerfile -t qareel-browser:dev .
```

Docker works the same way (`docker build -f host-linux/Containerfile -t qareel-browser:dev .`); `Containerfile.dockerignore` keeps the build context to the four source files.

Run the CLI against it with `QAREEL_LINUX_IMAGE=localhost/qareel-browser:dev qareel ...`. Release builds bake the published image in at compile time through the same variable, pinned by digest (`ghcr.io/patrick-lai/qareel-browser@sha256:...`).

## Contract

The image entrypoint is `/usr/local/bin/qareel-browser` and the image sets `QAREEL_BROWSER_CONTAINER=1`. The CLI starts it with `podman run --rm --interactive --init --pull=never ... --env HOME=<profile> --volume <profile>:<profile> <image>`, writes one bootstrap JSON line on stdin:

```json
{"profile_dir":"<profile>","profile_id":"<uuid>","ffmpeg":"/usr/bin/ffmpeg","pulseaudio":"/usr/bin/pulseaudio","dbus_daemon":"/usr/bin/dbus-daemon"}
```

and then speaks newline-delimited JSON. The host holds `<profile>/.qareel-browser.lock` while it runs.
