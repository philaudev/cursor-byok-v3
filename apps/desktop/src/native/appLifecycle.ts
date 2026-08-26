import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { api } from "../api";

export function hasNativeAppLifecycle(): boolean {
  return isTauri();
}

export async function currentAppVersion(): Promise<string> {
  return hasNativeAppLifecycle() ? getVersion() : "dev";
}

export async function readAutostart(): Promise<boolean> {
  return isEnabled();
}

export async function writeAutostart(enabled: boolean): Promise<void> {
  await (enabled ? enable() : disable());
}

export async function readSilentStart(): Promise<boolean> {
  return (await api.desktopSettings()).silent_start;
}

export async function writeSilentStart(silentStart: boolean): Promise<void> {
  await api.setDesktopSettings({ silent_start: silentStart });
}

export async function checkForUpdate(): Promise<Update | null> {
  return check();
}

export async function installUpdate(update: Update): Promise<void> {
  await update.downloadAndInstall();
  await relaunch();
}
