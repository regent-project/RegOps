# Release v0.16.0

## What's Changed

### Live view in regency

- With `Reporting = "Regency"`, each pass runs in regent-sdk's live mode: what it does
  (attributes, remediations, commands and their output, secrets redacted) is
  streamed to the endpoint while it runs (`ReportLive`, best effort: the pass never
  waits for it, and nothing is retried). New `[Regency]` key `live` (default
  `true`).
- The log of the pass (up to `run_log_max_bytes`, default 1 MiB, estimate; `0` =
  none) is sent with its outcome, through the spool, for regency's history.

### Other changes

- Updated following latest regent-sdk update (v0.25.0).

### Upgrade notes

- Needs a regency endpoint that knows `ReportLive` (otherwise the live view is just
  not shown; outcomes are reported as before).

## Installation

### Debian/Ubuntu
```bash
sudo dpkg -i ./regops_<version>_amd64.deb
```

### RHEL/CentOS/Fedora
```bash
sudo rpm -i ./regops-<version>.<arch>.rpm
```
