# Windows Packaging with Velopack

This guide explains how to turn the dashboard into a Windows installer using [Velopack](https://docs.velopack.io/).

## Prerequisites

- Windows 10 or later
- Rust toolchain with the `cargo` command available
- Velopack CLI installed (`dotnet tool install --global Velopack.Cli` or via MSI from the official site)
- Windows SDK (provides `signtool.exe`) if you plan to sign binaries

## Directory Layout

The repository now contains a `packaging/` directory with the following artifacts:

- `Velopack.toml` – CLI configuration that describes the bundle contents and metadata
- `package.ps1` – helper script that builds the dashboard and invokes `vpk pack`
- `bundle/` and `dist/` – generated at packaging time (ignored by git)

## Building the Dashboard

```powershell
cargo build --release --bin dashboard
```

The compiled executable will be placed at `target\release\dashboard.exe`.

## Creating an Installer

Run the helper script from PowerShell (it works from any directory inside the repo):

```powershell
.\packaging\package.ps1
```

What the script does:

1. Builds the `dashboard` binary in release mode (skip with `-SkipBuild` if you already produced it)
2. Copies the binary into `packaging\bundle\Star Citizen Playtime.exe`
3. Reads `packaging/Velopack.toml` for metadata and invokes `vpk pack` with the required parameters

The Velopack artifacts are written to `packaging\dist`. Typical outputs include:

- `Star Citizen Playtime.Setup.exe` (recommended installer)
- `Star Citizen Playtime-<version>-portable.zip` (portable build)

The exact filenames match the Velopack defaults and can be customized in `Velopack.toml`.

### Optional: Sign the binaries (self-signed or CA-issued)

The packaging script accepts an optional `-SignThumbprint` argument. When supplied, it:

1. Signs every `.exe` staged into `packaging\bundle` using `signtool`
2. Passes the same parameters to `vpk pack --signParams …` so the generated installer/portable binaries are signed as well

Example with a certificate in the **LocalMachine** store:

```powershell
./packaging/package.ps1 -SignThumbprint "YOUR_THUMBPRINT" -UseLocalMachineStore
```

Override the timestamp server or signtool path as needed:

```powershell
./packaging/package.ps1 -SignThumbprint "YOUR_THUMBPRINT" `
    -UseLocalMachineStore `
    -TimestampUrl "http://timestamp.sectigo.com" `
    -SignToolPath "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
```

> ℹ️ Self-signed certificates only avoid SmartScreen warnings on machines that trust the issuing root (typically just your own). Distribute that root certificate to target machines if you require trust outside your PC.
> Run the packaging script from an elevated PowerShell window when signing with a LocalMachine certificate so the process can access the private key.

### Creating a local code-signing certificate

Open an **elevated** PowerShell window and run:

```powershell
$cert = New-SelfSignedCertificate -Type CodeSigningCert `
    -Subject "CN=Star Citizen Playtime Local" `
    -CertStoreLocation "Cert:\LocalMachine\My"
```

Record the thumbprint:

```powershell
Get-ChildItem Cert:\LocalMachine\My | `
    Where-Object Subject -like '*Star Citizen Playtime Local*' | `
    Select-Object Thumbprint, Subject
```

Use that thumbprint with the `package.ps1` arguments shown above.

## Updating Metadata

`packaging/Velopack.toml` seeds sensible defaults for the current project release. The PowerShell script reads this file to fill in the Velopack CLI arguments, so keep it up to date when cutting a new version:

- `package.version` – match the crate version in `Cargo.toml`
- `package.summary` – optional release notes blurb
- `build.copy[...]` – add/remove files you want to ship (icons, readme, etc.). The script mirrors every `[[build.copy]]` entry into the staging `bundle/` directory before packing.

You can also provide a signed icon by setting `win.icon` once an `.ico` asset is available.

## Troubleshooting

- **`vpk` not found** – ensure the Velopack CLI is on your `PATH` (restart the shell after installing the dotnet tool).
- **Installer missing files** – confirm the paths in `[[build.copy]]` point to compiled artifacts. The entries are resolved relative to `packaging/` and copied verbatim into the bundle.
- **CLI demands `--packId`/`--packVersion`** – you are probably running `vpk` manually. Prefer `./packaging/package.ps1`, which reads the TOML and supplies the required arguments automatically.
- **Signature failures** – verify `signtool.exe` exists at `-SignToolPath`, the thumbprint is correct, and (for LocalMachine certificates) the script runs in an elevated shell or the account has permission to access the private key.
- **SmartScreen warnings** – distribute a code-signed installer or sign the Velopack outputs before publishing.

Refer to the official Velopack documentation for advanced options such as delta updates, custom channels, and release hosting strategies.
