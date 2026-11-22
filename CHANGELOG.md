# Release v0.17.0

## What's Changed

### Fleet model in regency

- With `Reporting = "Regency"`, each outcome also tells regency where the pass's
  expected state comes from (`[Git]` repository URL without credentials, branch and
  file, besides the commit already sent) and the expected state itself (JSON).
  regency shows whether the host runs the latest commit of its repository and
  whether its state matches the one regency assigns to it.
- New `[Regency]` key `labels` (e.g. `labels = { env = "prod", role = "web" }`):
  the host's labels, sent with every outcome; regency puts the host in the
  inventories (and assignments) they select. Keys are `[a-zA-Z_][a-zA-Z0-9_]*`,
  except `host` and `source`.

### Other changes

- Updated following latest regent-sdk update (v0.26.0).

## Installation

### Debian/Ubuntu
```bash
sudo dpkg -i ./regops_<version>_amd64.deb
```

### RHEL/CentOS/Fedora
```bash
sudo rpm -i ./regops-<version>.<arch>.rpm
```
