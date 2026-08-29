# Release v0.2.0

Initial release of RegOps agent with several fixes for bugs, deployment and permissions issues.

## What it does
* Installs RegOps package
* Creates its user and system service

## Installation
Download the `.deb` and install it using:
```bash
sudo apt install ./regops_<version>_amd64.deb
```

## Configuration
The configuration file is `/etc/regops/config.toml`.

When first installing, you must set the repository in this file.
Default mode is Assess.

After every configuration change, restart the regops service to take it into account :
```bash
systemctl restart regops
```

Logs are available using regular systemd commands :
```bash
journalctl -xeu regops --follow
```