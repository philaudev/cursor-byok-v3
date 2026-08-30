import { useEffect, useRef } from "react";
import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import { MessageProvider } from "./shared/ui/MessageProvider";
import { useMessage } from "./shared/ui/message";
import { AppFrame } from "./shell/AppFrame";
import { AppLayout } from "./shell/AppLayout";
import { CallsPage } from "./features/calls/CallsPage";
import { CallDetailsPage } from "./features/calls/CallDetailsPage";
import { CompactionConfigPage } from "./features/calls/CompactionConfigPage";
import { CursorSettingsPage } from "./features/models/CursorSettingsPage";
import { HomePage } from "./features/home/HomePage";
import { PluginManagementPage } from "./features/plugins/PluginManagementPage";
import { SettingsPage } from "./features/settings/SettingsPage";
import { useAppStore } from "./shared/store/appStore";
import { updateStore } from "./shared/store/updateStore";

export function App() {
  return (
    <>
      <HashRouter>
        <Routes>
          <Route path="calls/:callId" element={<CallDetailsPage />} />
          <Route element={<AppFrame />}>
            <Route element={<AppLayout />}>
              <Route index element={<HomePage />} />
              <Route path="calls" element={<CallsPage />} />
              <Route path="harness/cursor" element={<CursorSettingsPage />} />
              <Route path="config" element={<CompactionConfigPage />} />
              <Route path="plugins" element={<PluginManagementPage />} />
              <Route path="settings" element={<SettingsPage />} />
            </Route>
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </HashRouter>
      <AppMessages />
    </>
  );
}

function AppMessages() {
  const { error } = useAppStore();
  const previousError = useRef<string | null>(null);
  const showMessage = useMessage();

  useEffect(() => {
    if (error && error !== previousError.current) showMessage(error);
    previousError.current = error;
  }, [error, showMessage]);

  useEffect(() => {
    void updateStore.check().then((version) => {
      if (!version) return;
      showMessage(t("发现新版本 {version}，可在设置中安装", { version }), { duration: 6_000 });
    }).catch(() => {
      // Startup checks are best-effort; manual checks in Settings report errors.
    });
  }, [showMessage]);

  return <MessageProvider />;
}
