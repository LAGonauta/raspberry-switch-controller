# Raspberry Switch Controller

Bridge Xbox wireless controllers to a Nintendo Switch over USB OTG gadget mode on Raspberry Pi 4.

## Cross-Compilation for Raspberry Pi 4

To build for RPi4 (aarch64):

```bash
CROSS_CONTAINER_ENGINE=podman cross build --target aarch64-unknown-linux-gnu --release
```

The binary will be at:
```
target/aarch64-unknown-linux-gnu/release/raspberry-switch-controller
```

### Prerequisites
- Install cross: `cargo install cross --git https://github.com/cross-rs/cross`
- Podman or Docker must be available
- If running cross inside a container, set `CROSS_CONTAINER_IN_CONTAINER=true`
