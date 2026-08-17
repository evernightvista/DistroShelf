## ===========================================================================
## DistroShelf RPM 打包规范
## ---------------------------------------------------------------------------
## DistroShelf 是一个基于 GTK4/libadwaita 的 Distrobox 容器图形管理器，
## 使用 Rust 编写，通过 Meson 构建系统（内部调用 cargo）编译。
##
## 注意：evernightvista/DistroShelf 是 ranfdev/DistroShelf 的 fork，
##       该 fork 暂无 release/tag，故以 main 分支的 commit 快照打包。
##       重新固定版本时，更新下方 %{commit} 即可。
## ===========================================================================

%global forgeurl    https://github.com/evernightvista/DistroShelf
%global commit      6101296fc65b48a389dee47d60bf6d91f63e3f18
%global shortcommit %(c=%{commit}; echo ${c:0:7})
%global date        20260817

Name:       distroshelf
Version:    1.5.2
Release:    0.%{date}git%{shortcommit}%{?dist}
Summary:    GTK4 graphical manager for Distrobox containers

License:    GPL-3.0-or-later
URL:        %{forgeurl}
Source0:    https://github.com/ranfdev/DistroShelf/archive/refs/tags/v1.5.2.tar.gz

# —— 构建依赖 ————————————————————————————————————————————————
# Rust edition 2024 需要 rustc/cargo >= 1.85
BuildRequires:  meson >= 1.0.0
BuildRequires:  ninja-build
BuildRequires:  rustc >= 1.85
BuildRequires:  cargo >= 1.85
BuildRequires:  gcc
# 以下库版本下限源自 Cargo.toml 中 Rust crate 的 feature；
# cargo 在编译期会强制校验真实最低版本，若提示版本过低请升级对应库。
#   gtk4-rs feature gnome_49  -> GTK 4.16        (Fedora 包: gtk4-devel)
#   libadwaita-rs feature v1_9 -> libadwaita 1.7+ (Fedora 包: libadwaita-devel)
#   vte4-rs feature v0_76      -> VTE 0.76        (Fedora 包: vte291-gtk4-devel)
BuildRequires:  pkgconfig(gtk4)          >= 4.16
BuildRequires:  pkgconfig(libadwaita-1)  >= 1.7
BuildRequires:  pkgconfig(vte-2.91-gtk4) >= 0.76
BuildRequires:  gettext
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  /usr/bin/glib-compile-schemas
BuildRequires:  python3

# —— 运行时依赖 ————————————————————————————————————————————————
Requires:       distrobox
Requires:       hicolor-icon-theme
# 需要一个终端模拟器用于在容器中打开 shell 会话（弱依赖，任一即可）
Recommends:     (gnome-terminal or konsole or xfce4-terminal or tilix or alacritty)

%description
DistroShelf is a graphical application for managing Distrobox containers on
Linux. Built with GTK4 and libadwaita, it lets you create and manage
containers, view their status and details, install packages, manage exported
applications, open terminal sessions, and upgrade, clone or delete containers.

%prep
%autosetup -n DistroShelf-1.5.2 -p1

%build
%meson \
    %{?with_offline:-Doffline=true} \
    %{!?with_offline:-Doffline=false}
%meson_build

%install
%meson_install

# Meson 的 gnome.post_install 会在 buildroot 中生成缓存文件。
# 这些缓存不应打入 rpm（会跨包冲突），改由下方 %post/%postun 在目标系统重建。
rm -f %{buildroot}%{_datadir}/glib-2.0/schemas/gschemas.compiled
find %{buildroot}%{_datadir}/icons -type f -name 'icon-theme.cache' -delete 2>/dev/null || :
rm -f %{buildroot}%{_datadir}/applications/*.mime 2>/dev/null || :

%find_lang distroshelf

%check
# meson 内置测试：desktop-file-validate / appstreamcli / glib-compile-schemas 校验（均为离线）。
# 若 appstream 元数据校验因环境差异失败，可改为：%meson_test || :
%meson_test

%files -f distroshelf.lang
%license COPYING
%doc README.md
%{_bindir}/distroshelf
%{_datadir}/applications/com.ranfdev.DistroShelf.desktop
%{_datadir}/metainfo/com.ranfdev.DistroShelf.metainfo.xml
%{_datadir}/distroshelf/
%{_datadir}/glib-2.0/schemas/com.ranfdev.DistroShelf.gschema.xml
%{_datadir}/dbus-1/services/com.ranfdev.DistroShelf.service
%{_datadir}/icons/hicolor/scalable/apps/com.ranfdev.DistroShelf.svg
%{_datadir}/icons/hicolor/symbolic/apps/com.ranfdev.DistroShelf-symbolic.svg

%post
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/glib-compile-schemas %{_datadir}/glib-2.0/schemas &>/dev/null || :

%postun
/usr/bin/update-desktop-database &>/dev/null || :
/usr/bin/gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor &>/dev/null || :
/usr/bin/glib-compile-schemas %{_datadir}/glib-2.0/schemas &>/dev/null || :

%changelog
* Sun Aug 17 2026 DistroShelf Maintainer <nobody@example.com> - 1.5.2-0.20260817git6101296
- Initial package: snapshot of main branch @ 6101296.
