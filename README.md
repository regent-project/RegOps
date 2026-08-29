# RegOps

A lightweight GitOps agent written in Rust and built on top of the `regent-sdk` crate, designed for automated and reliable local configuration management.

## Features

* **Decentralized Configuration Management:** Hosts independently manage and enforce their own configurations, combining autonomous local execution with the control, version history, and auditability of a central Git repository.
* **System Service Native:** Full systemd integration for seamless background execution, enabling easy startup, shutdown, and persistence (`systemctl start regops`).
* **Standardized Logging:** Automatic log capture via systemd (`journalctl -u regops`), removing the need for manual file management.

## The reconciliation loop (dead simple)

```mermaid
graph TD
    Start([Start Agent Loop]) --> Pull[Pull Expected State YAML from Remote Git Repo]
    Pull --> Apply[Assess or Enforce Expected Configuration to Localhost]
    Apply --> Sleep[Sleep for X Seconds (30 by default)]
    Sleep --> Start

## Contributing

We welcome contributions and feedback ! This project needs help with:

- **Linux distributions integration** : Right now we only ship for Debian-based distributions. We want to extend our CI to Fedora/CentOS/RHEL ecosystem, and others as well !

**Join the conversation** [Regent Discord](https://discord.gg/2gxAW7uzsx)


## License

RegOps is licensed under the [Apache License, Version 2.0](LICENSE).