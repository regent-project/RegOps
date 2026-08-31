# Release v0.2.1

Adding token-based authentication for remote git repository (gitlab and github compatiblity using "oauth2" key)

## Update
Download the `.deb` and install it using:
```bash
sudo dpkg -i ./regops_<version>_amd64.deb
```

## Configuration
To add an authentication token, under the Git section of the configuration, add :
```toml
[Git.auth]
token = "XXXX"
```