# Package repository

APT, YUM and APK repositories with every released version, served as static
files from GitHub Pages at `https://bigleka.github.io/pgdog`. Install
instructions for users are in [../README.md](../README.md#install).

```
pgdog.asc                     GPG public key (APT and YUM)
pgdog-bigleka.rsa.pub         RSA public key (APK)
pgdog.repo                    dnf repository definition
deb/dists/stable/InRelease    APT index, signed; Release + Release.gpg too
deb/pool/main/*.deb
rpm/{x86_64,aarch64}/         packages + repodata/, repomd.xml.asc signed
apk/{x86_64,aarch64}/         packages + APKINDEX.tar.gz, signed
```

What is signed, and what checks it:

| | Signed by | Checked by |
|-|-----------|------------|
| APT `InRelease` | GPG key | `apt update` (`signed-by=`) |
| `.rpm` packages | GPG key, by nfpm | `dnf` (`gpgcheck=1`) |
| YUM `repomd.xml` | GPG key | `dnf` (`repo_gpgcheck=1`) |
| `.apk` packages | RSA key, by nfpm | `apk add` of a local file |
| `APKINDEX.tar.gz` | RSA key | `apk update` / `apk add` |

Alpine doesn't use GPG, hence two keys. `.deb` files aren't signed
individually: as in Debian itself, apt trusts the signed index, which holds
every package's SHA256.

## Keys

Fingerprint of the GPG key: _(fill in when the key is created)_.

Created once, kept by the maintainer and stored as two repository secrets.
Neither has a passphrase: CI signs unattended, and the secrets are what
protects them. Run in a scratch directory and delete it afterwards:

```bash
export GNUPGHOME="$(mktemp -d)"
gpg --batch --passphrase '' --quick-gen-key \
    "PgDog packages (bigleka) <ricardo@leka.com.br>" rsa4096 sign 5y
gpg --armor --export-secret-keys > repo-gpg.asc
gpg --fingerprint                                  # goes in "Keys" above
openssl genrsa -out repo-apk.rsa 4096

gh secret set REPO_GPG_KEY < repo-gpg.asc
gh secret set REPO_APK_KEY < repo-apk.rsa
```

Keep an offline copy of both files: users trust these keys, and a new key
means every user re-imports it.

The GPG key expires after 5 years. Extend it before then
(`gpg --quick-set-expire <fingerprint> 5y`), update the secret and release:
the published `pgdog.asc` carries the new date. Expired, `apt` and `dnf`
refuse the repository.

## GitHub Pages

Once, in the repository settings:

1. Pages → Build and deployment → Source: **GitHub Actions**.
2. Environments → `github-pages` → Deployment branches and tags: add the
   tag rule `v*`. Pages deploys from the release tag; by default only the
   default branch may deploy.

## How it's built

On every pull request that touches packaging, the `release` workflow signs
the packages and builds and tests the repository with throwaway keys
([test-keys.sh](test-keys.sh)). On a `v*` tag it uses the secrets, refuses
to run without them, adds the packages of every earlier GitHub Release,
tests installing from the result on each distribution and architecture, and
deploys it to Pages after the release is created.

Locally (podman or docker):

```bash
packaging/repo/test-keys.sh keys    # needs gpg and openssl
NFPM_RPM_KEY_FILE=keys/gpg.asc NFPM_APK_KEY_FILE=keys/apk.rsa \
    packaging/package.sh amd64 dist/glibc/pgdog dist/musl/pgdog packages
REPO_URL=https://bigleka.github.io/pgdog \
REPO_GPG_KEY_FILE=keys/gpg.asc REPO_APK_KEY_FILE=keys/apk.rsa \
    packaging/repo/build.sh packages repo
REPO_URL=https://bigleka.github.io/pgdog packaging/test/repo.sh repo amd64
```

`repo.sh` installs from `repo/` with signature checks on, after checking
that a key that didn't sign the repository is refused.
