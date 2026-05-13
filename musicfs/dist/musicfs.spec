Name:           musicfs
Version:        0.1.0
Release:        1%{?dist}
Summary:        Metadata-Organized Music Filesystem

License:        MIT
URL:            https://github.com/yourusername/musicfs
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.70
BuildRequires:  cargo
BuildRequires:  fuse3-devel

Requires:       fuse3

%description
MusicFS is a virtual FUSE filesystem that organizes music files by metadata.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm755 target/release/musicfs %{buildroot}%{_bindir}/musicfs
install -Dm644 dist/musicfs.service %{buildroot}%{_unitdir}/musicfs.service
install -Dm644 config.example.toml %{buildroot}%{_sysconfdir}/musicfs/config.example.toml

%files
%license LICENSE
%doc README.md
%{_bindir}/musicfs
%{_unitdir}/musicfs.service
%config(noreplace) %{_sysconfdir}/musicfs/config.example.toml

%changelog
* Mon Jan 01 2024 MusicFS Team <team@example.com> - 0.1.0-1
- Initial package
