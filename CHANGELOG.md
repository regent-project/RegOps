# Release v0.1.1

Initial release of RegOps agent.

## What it does
* Installs RegOps package
* Creates its user and system service

## Installation
Download the `.deb` and install it using:
```bash
sudo apt install ./regops_0.1.0_amd64.deb
```

## Configuration
The configuration file is `/etc/regops/config.toml`.

When first installing, you must set the repository in this file.
When set, restart the regops service
```bash
systemctl restart regops
```