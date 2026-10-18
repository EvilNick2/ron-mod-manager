# Privacy Policy

Last updated: 18 October 2026

RON Mod Manager is a desktop app for managing Ready or Not mods, which runs
on your own computer. It doesn't send anything to me or to any third
party, apart from the services listed further down that it talks to on your
behalf.

## What's stored on your computer

Your settings, such as the game folder and mod storage folder, are saved in a
config file in your user profile. Mods and modpacks you install go in your mod
storage folder.

Your Nexus Mods API key is saved in your operating system's keyring, which is
Windows Credential Manager on Windows and the Secret Service on Linux. If you
sign in with Google, your Google sign-in token and account email address are
saved there too.

## Google account data

You only need to sign in with Google if you want to publish a modpack to your
own Google Drive.

When you sign in, the app asks for one permission, the
`https://www.googleapis.com/auth/drive.file` scope. This only lets it see and
change the files and folders it created itself, so it can't see or change
anything else in your Drive.

With that permission the app:

- creates a `RoNMM` folder in your Drive, with a folder inside it for each
  modpack you publish
- uploads your modpack's mod files and a `manifest.json` file into it, and
  deletes any files a newer version of the modpack doesn't need anymore
- shares each modpack folder as "anyone with the link can view", so your
  players can download it
- checks your Drive storage quota before uploading, to make sure there's
  enough space
- reads your Google account email address, so it can show which account is
  signed in

Your Google data is only used for these features. It isn't sold, used for
advertising or shared with anyone, and I never receive it. Use of information
received from Google APIs follows the
[Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements.

## Adding modpacks from a link

Players adding a modpack from a Google Drive link don't sign in. The app
downloads the shared files using its own API key, so no personal data is
involved.

## Services the app connects to

The app talks to Nexus Mods to sign in, browse mods and download them along
with their details. It talks to Google Drive only when you publish a modpack or
add one from a link, and to GitHub to check for and download app updates. Each
of these has its own privacy policy.

## Removing your data

Signing out of Google in the app's Settings deletes the stored Google token and
email address from your computer. You can also revoke the app's access at any
time at https://myaccount.google.com/permissions.

Uninstalling the app and deleting its config and mod storage folders removes
everything else it stored. Files the app published to your Google Drive stay
there until you delete them in Drive yourself.

## Contact

If you have any questions about this policy, open an issue at
https://github.com/EvilNick2/ron-mod-manager/issues.
