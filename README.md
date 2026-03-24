<!-- markdownlint-disable MD033 -->
<h1 align="center">RoN Mod Manager</h1>

<p align="center">
	<img alt="Github top language" src="https://img.shields.io/github/languages/top/EvilNick2/ron-mod-manager?color=56BEB8">
	<img alt="Github language count" src="https://img.shields.io/github/languages/count/EvilNick2/ron-mod-manager?color=56BEB8">
	<img alt="Repository size" src="https://img.shields.io/github/repo-size/EvilNick2/ron-mod-manager?color=56BEB8">
	<img alt="License" src="https://img.shields.io/github/license/EvilNick2/ron-mod-manager?color=56BEB8">
</p>

<p align="center">
	<a href="#rocket-usage">Usage</a> &#xa0; | &#xa0;
	<a href="#dart-about">About</a> &#xa0; | &#xa0;
	<a href="#sparkles-features">Features</a> &#xa0; | &#xa0;
	<a href="#computer-development">Development</a> &#xa0; | &#xa0;
	<a href="#rocket-technologies">Technologies</a> &#xa0; | &#xa0;
	<a href="#memo-license">License</a> &#xa0; | &#xa0;
	<a href="https://github.com/EvilNick2" target="_blank">Author</a>
</p>

<br>
<!-- markdownlint-enable MD033 -->

## :rocket: Usage ##

This section is for **end users** who want to manage their Ready or Not mods using a clean, performant GUI.  
You **do not** need Node.js, Rust, or any developer tools to run the application.

---

### 1. Download the Application ###

- Go to the **[Releases](https://github.com/EvilNick2/ron-mod-manager/releases)** page
- Download the installer for your operating system:
	- **Windows**: `.msi`
	- **Linux**: `.AppImage`, `.deb`, or `.rpm`

> :warning: **Windows Antivirus Notice**  
> Because this is a custom-built Tauri application, some antivirus software may incorrectly flag the binary.  
> If this occurs, add an exception.

## :dart: About ##

A modern, high-performance mod manager for **Ready or Not**. Built with Tauri and Vue, it provides a seamless experience for installing, enabling, and managing your local mods, alongside deep integration with Nexus Mods for online browsing. It uses a virtualized symlink system to enable/disable mods instantly without moving gigabytes of data.

## :sparkles: Features ##

:heavy_check_mark: **Instant Toggle**: Enable/disable mods via symlinks instantly\
:heavy_check_mark: **Nexus Mods Integration**: Browse, search, and view stats (downloads, endorsements) directly in-app\
:heavy_check_mark: **Mod Presets**: Save, load, and share collections of enabled mods via portable ZIP exports\
:heavy_check_mark: **Drag-and-Drop Installation**: Drop any mod ZIP directly into the grid to install\
:heavy_check_mark: **Performance First**: Optimized UI for smooth scrolling on Linux/WebKitGTK with large mod libraries\
:heavy_check_mark: **Cross-platform**: Built with Tauri for native performance on Windows and Linux

---

## :computer: Development ##

The sections below are **only** required if you are developing or modifying the application.

### :white_check_mark: Requirements ###

- [Git](https://git-scm.com/)
- [Node.js](https://nodejs.org/en)
- [Rust Language Toolchain](https://www.rust-lang.org/tools/install)
- Tauri Prerequisites (See the [Tauri Guide](https://tauri.app/start/prerequisites/))

### :checkered_flag: Starting ###

1. **Clone the repository:**
	```bash
	git clone https://github.com/EvilNick2/ron-mod-manager
	cd ron-mod-manager
	```

2. **Install frontend dependencies:**
	```bash
	npm install
	```

3. **Run in development mode:**
	```bash
	npm run tauri dev
	```

4. **Build:**
	```bash
	npm run tauri build
	```

## :rocket: Technologies ##

- [Tauri v2](https://v2.tauri.app/)
- [Rust](https://rust-lang.org/)
- [Vue.js 3](https://vuejs.org/)
- [Vite](https://vite.dev/)
- [TypeScript](https://www.typescriptlang.org/)

## :memo: License ##

This project is under the MIT license. See the [LICENSE](LICENSE) file.

Made by [EvilNick2](https://github.com/EvilNick2)

[Back to top](#top)
