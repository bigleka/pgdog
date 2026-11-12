#!/bin/sh
# APT and YUM indexes. Runs in a Debian container started by build.sh:
# /pkgs packages, /out the repository, /keys/gpg.asc the signing key.
set -eu

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends apt-utils createrepo-c gnupg >/dev/null

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
gpg --batch --quiet --import /keys/gpg.asc
fingerprint=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
sign() { gpg --batch --yes --local-user "$fingerprint" "$@"; }
echo "signing with GPG key $fingerprint"
gpg --batch --armor --export "$fingerprint" > /out/pgdog.asc

# APT: one suite, "stable", with every version in a flat pool.
mkdir -p /out/deb/pool/main
cp /pkgs/*.deb /out/deb/pool/main/
cd /out/deb
for arch in amd64 arm64; do
    dir=dists/stable/main/binary-$arch
    mkdir -p "$dir"
    apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
    gzip -9kn "$dir/Packages"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=PgDog \
    -o APT::FTPArchive::Release::Label=PgDog \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    -o APT::FTPArchive::Release::Description="PgDog packages" \
    release dists/stable > dists/stable/Release
sign --clearsign --output dists/stable/InRelease dists/stable/Release
sign --armor --detach-sign --output dists/stable/Release.gpg dists/stable/Release

# YUM: one repository per architecture; nfpm signed the packages.
for arch in x86_64 aarch64; do
    mkdir -p "/out/rpm/$arch"
    find /pkgs -name "*.$arch.rpm" -exec cp {} "/out/rpm/$arch/" \;
    createrepo_c --quiet "/out/rpm/$arch"
    sign --armor --detach-sign --output "/out/rpm/$arch/repodata/repomd.xml.asc" \
        "/out/rpm/$arch/repodata/repomd.xml"
done
cat > /out/pgdog.repo <<EOF
[pgdog]
name=PgDog
baseurl=$REPO_URL/rpm/\$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$REPO_URL/pgdog.asc
EOF

chown -R "$OWNER" /out
