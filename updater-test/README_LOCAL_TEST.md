# Local Updater Test Guide

This directory contains files used **only for local updater testing**.  
Although it is included in the repository, nothing here is used in production builds or GitHub Actions unless explicitly referenced.

> **Important**  
> The Tauri updater **only runs in built applications**.  
> The updater does **not** execute in `tauri dev`. All tests described here require a **built installer**.

---

## Purpose

The local updater environment allows you to:

- Serve a custom `latest.json` using a local HTTP server  
- Test update downloads and installations  
- Validate the Tauri updater flow end-to-end in a real build  
- Use a separate Tauri configuration without affecting production behavior  

This folder exists to support reproducible local testing for anyone cloning the repository.

---

## Folder Structure

```
updater-test/
│
├── latest.json                # Local update manifest
├── installer file             # Test installer (name may vary)
│
├── server.py                  # Local HTTP server for serving update files
├── tauri.local.conf.json      # Tauri override config used only in local testing
└── README_LOCAL_TEST.md       # This file
```

---

## 1. Run the Local HTTP Server

From the project root:

```bash
python updater-test/server.py
```

The server exposes the `updater-test` directory at:

```
http://localhost:8000/
```

Verify file availability:

- http://localhost:8000/latest.json

This must return the requested file.

---

## 2. Build the Application Using the Local Updater Configuration

Because the updater only works in built applications, you must create a build using the local override config:

```bash
npm run build:local
```

This produces a local installer that is configured to:

- Read updates from `http://localhost:8000/latest.json`
- Allow HTTP (insecure transport) for local testing only

> **Note**  
> Production builds *always* use `tauri.conf.json`.  
> The local override config is never used unless explicitly specified.

---

## 3. Install and Run the Built App

After building:

1. Locate the generated installer in:
   ```
   src-tauri/target/release/bundle/
   ```
2. Install the application normally.
3. Launch the installed app.

On startup (or when the updater is triggered), the app will check the local updater server.

---

## 4. Editing `latest.json`

Example template:

```json
{
  "version": "1.0.1",
  "notes": "Temporarily remove Logs page as it's not yet implemented",
  "pub_date": "2025-12-06T06:44:43.919Z",
  "platforms": {
    "windows-x86_64-nsis": {
      "signature": "<BASE64_SIGNATURE>",
      "url": "http://localhost:8000/v1.0.1/RoN.Mod.Manager_1.0.1_x64-setup.exe"
    }
  }
}
```

Important:

- `"version"` must be **greater** than the currently installed app version  
- `"url"` must point to a file served by the local HTTP server  
- `"signature"` must be valid for installation testing  

---

## 5. Updating the Test Installer

To test a new update:

1. Build a new installer with a higher version number  
2. Place the installer inside `updater-test/` (or a subfolder)  
3. Update the `url`, `version`, and `signature` fields in `latest.json`  
4. Restart the local HTTP server if needed  
5. Launch the already-installed app to trigger the update  

---

## 6. Notes

- This folder is tracked in Git so collaborators can test the updater consistently  
- Installer binaries are ignored via `updater-test/.gitignore`  
- GitHub Actions does **not** use this folder unless explicitly configured  
- The production updater hosted on GitHub Pages is completely independent of this setup  
