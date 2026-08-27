export type DesktopPlatform = "macos" | "windows" | "linux";

export function desktopPlatform(): DesktopPlatform {
  const agent = navigator.userAgent;
  if (/Macintosh|Mac OS X/.test(agent)) return "macos";
  if (/Windows/.test(agent)) return "windows";
  return "linux";
}
