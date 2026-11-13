# magic

Moves my Magic Keyboard, Mouse and Trackpad between my Macs, because Apple won't let you just switch them.

Run `magic` to see every device and which Mac it's on. Pick one, press a number to send it to that Mac, or Enter to pull it over here. `magic m5` sends everything in one go.

It works by unpairing on one Mac and pairing on the other, using [blueutil](https://github.com/toy/blueutil) over SSH. You need `brew install blueutil`, Bluetooth permission for `/usr/libexec/sshd-keygen-wrapper` on each Mac, and `magic setup` run once on each.

Sometimes a move leaves a device on neither Mac. Switch it off and on and run `magic rescue`, or plug it into a Mac for a few seconds.

The Macs are hardcoded in `src/host.rs`, or put your own in `~/.config/magic/hosts` as `name user@host` lines.

```
cargo install --path .
```
