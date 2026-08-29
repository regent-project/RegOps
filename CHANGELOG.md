# Release v0.1.4

Initial release of RegOps agent.

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

After every configuration change, restart the regops service to take it into account :
```bash
systemctl restart regops
```
