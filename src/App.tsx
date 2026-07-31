import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  check,
  type DownloadEvent,
  type Update,
} from "@tauri-apps/plugin-updater";
import {
  Bluetooth,
  Check,
  ChevronRight,
  ClipboardCopy,
  Copy,
  Download,
  File,
  Folder,
  FolderOpen,
  FolderUp,
  Inbox,
  Laptop,
  Link2,
  LoaderCircle,
  Radio,
  RefreshCw,
  Search,
  Send,
  Settings,
  ShieldCheck,
  Trash2,
  Unlink,
  Wifi,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import packageMetadata from "../package.json";
import nearweaveIcon from "../src-tauri/icons/128x128.png";
import "./App.css";

type TransferState =
  | "queued"
  | "transferring"
  | "waiting_for_peer"
  | "cancelling"
  | "canceled"
  | "completed"
  | "failed";

type ConnectionServiceState =
  | "starting"
  | "running"
  | "stopping"
  | "stopped";

interface NearbyDevice {
  id: string;
  deviceId: string | null;
  name: string;
  bluetoothEndpoint: string | null;
  lanEndpoint: string | null;
  bluetoothPaired: boolean;
  nearweaveEnabled: boolean;
  trusted: boolean;
}

type ServiceStatus = "off" | "ready" | "error";

interface ListenerStatus {
  bluetooth: ServiceStatus;
  discovery: ServiceStatus;
  tcp: ServiceStatus;
  bluetoothError: string | null;
  discoveryError: string | null;
  tcpError: string | null;
  tcpPort: number | null;
  localAddresses: string[];
}

interface PairingRequest {
  requestId: string;
  deviceId: string;
  deviceName: string;
  verificationCode: string;
}

interface TrustedDevice {
  deviceId: string;
  name: string;
  fingerprint: string;
  lastSeenAt: number;
}

interface LocalShare {
  id: string;
  name: string;
  path: string;
}

interface SharedRoot {
  id: string;
  name: string;
}

interface RemoteEntry {
  shareId: string;
  shareName: string;
  relativePath: string;
  name: string;
  size: number;
  isDirectory: boolean;
}

interface RemoteWorkspace {
  deviceId: string;
  roots: SharedRoot[];
}

interface DirectoryPage {
  deviceId: string;
  shareId: string;
  relativePath: string;
  offset: number;
  nextOffset: number | null;
  entries: RemoteEntry[];
}

interface PeerConnection {
  deviceId: string;
  name: string;
  connectionKind: "bluetooth" | "lan";
  bluetoothConnected: boolean;
  networkConnected: boolean;
  reconnecting: boolean;
}

interface Transfer {
  id: string;
  peerDeviceId: string;
  peerName: string;
  name: string;
  direction: "sending" | "receiving";
  state: TransferState;
  bytesDone: number;
  totalBytes: number;
  detail: string;
  bytesPerSecond: number;
  elapsedMillis: number;
  estimatedRemainingSeconds: number | null;
}

interface AppSnapshot {
  platform: string;
  deviceId: string;
  deviceName: string;
  listening: boolean;
  connectionServiceState: ConnectionServiceState;
  listenerStatus: ListenerStatus;
  connected: boolean;
  networkConnected: boolean;
  activeLink: "none" | "bluetooth" | "network";
  reconnecting: boolean;
  peerName: string | null;
  connectionKind: "bluetooth" | "lan" | null;
  pairingRequest: PairingRequest | null;
  pairingRequests: PairingRequest[];
  connections: PeerConnection[];
  trustedDevices: TrustedDevice[];
  clipboardEnabled: boolean;
  autostartEnabled: boolean;
  receiveDirectory: string;
  legacyReceiveDirectory: string | null;
  devices: NearbyDevice[];
  localShares: LocalShare[];
  remoteWorkspaces: RemoteWorkspace[];
  transfers: Transfer[];
}

interface Notice {
  level: "info" | "success" | "error";
  message: string;
}

type UpdatePhase =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "downloading"
  | "installing"
  | "error";

interface UpdateSnapshot {
  phase: UpdatePhase;
  currentVersion: string;
  availableVersion: string | null;
  notes: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
}

const isDevelopmentMock =
  import.meta.env.DEV &&
  new URLSearchParams(window.location.search).has("mock");

const initialSnapshot: AppSnapshot = {
  platform: "windows",
  deviceId: "",
  deviceName: "此电脑",
  listening: false,
  connectionServiceState: "stopped",
  listenerStatus: {
    bluetooth: "off",
    discovery: "off",
    tcp: "off",
    bluetoothError: null,
    discoveryError: null,
    tcpError: null,
    tcpPort: null,
    localAddresses: [],
  },
  connected: false,
  networkConnected: false,
  activeLink: "none",
  reconnecting: false,
  peerName: null,
  connectionKind: null,
  pairingRequest: null,
  pairingRequests: [],
  connections: [],
  trustedDevices: [],
  clipboardEnabled: true,
  autostartEnabled: false,
  receiveDirectory: "",
  legacyReceiveDirectory: null,
  devices: [],
  localShares: [],
  remoteWorkspaces: [],
  transfers: [],
};

function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>(initialSnapshot);
  const [busy, setBusy] = useState<string | null>("startup");
  const [notice, setNotice] = useState<Notice | null>(null);
  const [remoteQuery, setRemoteQuery] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [manualEndpoint, setManualEndpoint] = useState("");
  const [fileDropActive, setFileDropActive] = useState(false);
  const [selectedPeerId, setSelectedPeerId] = useState<string | null>(null);
  const [directoryPage, setDirectoryPage] = useState<DirectoryPage | null>(
    null,
  );
  const [updater, setUpdater] = useState<UpdateSnapshot>({
    phase: "idle",
    currentVersion: "",
    availableVersion: null,
    notes: null,
    downloadedBytes: 0,
    totalBytes: null,
    error: null,
  });
  const pendingUpdate = useRef<Update | null>(null);
  const updaterMounted = useRef(true);
  const selectedPeerRef = useRef<string | null>(null);
  const serviceStateRef = useRef<ConnectionServiceState>("stopped");
  const dropDedupRef = useRef(new Map<string, number>());

  const selectedConnection =
    snapshot.connections.find(
      (connection) => connection.deviceId === selectedPeerId,
    ) ?? null;
  const selectedWorkspace =
    snapshot.remoteWorkspaces.find(
      (workspace) => workspace.deviceId === selectedPeerId,
    ) ?? null;
  const selectedRoot =
    selectedWorkspace?.roots.find(
      (root) => root.id === directoryPage?.shareId,
    ) ?? null;
  const directorySegments = directoryPage?.relativePath
    ? directoryPage.relativePath.split("/").filter(Boolean)
    : [];
  const activePairing =
    snapshot.pairingRequests[0] ?? snapshot.pairingRequest;
  const serviceRunning = snapshot.connectionServiceState === "running";
  const serviceTransitioning =
    snapshot.connectionServiceState === "starting" ||
    snapshot.connectionServiceState === "stopping";

  useEffect(() => {
    setSelectedPeerId((current) => {
      if (
        current &&
        snapshot.connections.some(
          (connection) => connection.deviceId === current,
        )
      ) {
        return current;
      }
      return snapshot.connections[0]?.deviceId ?? null;
    });
  }, [snapshot.connections]);

  useEffect(() => {
    selectedPeerRef.current = selectedPeerId;
    serviceStateRef.current = snapshot.connectionServiceState;
    setDirectoryPage((current) =>
      current?.deviceId === selectedPeerId ? current : null,
    );
  }, [selectedPeerId, snapshot.connectionServiceState]);

  useEffect(() => {
    let active = true;
    let unlistenState: UnlistenFn | undefined;
    let unlistenNotice: UnlistenFn | undefined;

    Promise.all([
      listen<AppSnapshot>("nearweave://state", (event) => {
        if (active) setSnapshot(event.payload);
      }).then((cleanup) => {
        unlistenState = cleanup;
      }),
      listen<Notice>("nearweave://notice", (event) => {
        if (active) setNotice(event.payload);
      }).then((cleanup) => {
        unlistenNotice = cleanup;
      }),
      invoke<AppSnapshot>("get_snapshot").then((value) => {
        if (active) setSnapshot(value);
      }),
    ])
      .catch((error) => {
        if (active) {
          setNotice({ level: "error", message: normalizeError(error) });
        }
      })
      .finally(() => {
        if (active) setBusy(null);
      });

    return () => {
      active = false;
      unlistenState?.();
      unlistenNotice?.();
    };
  }, []);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 4200);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    updaterMounted.current = true;
    if (isDevelopmentMock) {
      setUpdater((current) => ({
        ...current,
        phase: "current",
        currentVersion: `${packageMetadata.version}-dev`,
      }));
      return;
    }

    void getVersion()
      .then((version) => {
        if (updaterMounted.current) {
          setUpdater((current) => ({ ...current, currentVersion: version }));
        }
      })
      .catch(() => undefined);

    const timer = window.setTimeout(() => {
      void checkForUpdates(false);
    }, 1500);
    return () => {
      updaterMounted.current = false;
      window.clearTimeout(timer);
      const update = pendingUpdate.current;
      pendingUpdate.current = null;
      void update?.close();
    };
  }, []);

  useEffect(() => {
    let active = true;
    let unlisten: UnlistenFn | undefined;

    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (!active) return;
        if (
          event.payload.type === "enter" ||
          event.payload.type === "over"
        ) {
          setFileDropActive(true);
          return;
        }
        if (event.payload.type === "leave") {
          setFileDropActive(false);
          return;
        }
        if (event.payload.type !== "drop") return;

        setFileDropActive(false);
        const paths = event.payload.paths;
        const deviceId = selectedPeerRef.current;
        if (serviceStateRef.current !== "running") {
          setNotice({
            level: "info",
            message: "连接服务已停止，请先恢复连接",
          });
          return;
        }
        if (!deviceId) {
          setNotice({
            level: "info",
            message: "请先选择已连接设备，再拖拽文件发送",
          });
          return;
        }
        if (!paths.length) return;

        const normalizedPaths = [...new Set(paths)]
          .map((path) => path.replace(/\//g, "\\").toLocaleLowerCase())
          .sort();
        const dedupKey = `${deviceId}:${normalizedPaths.join("\u0000")}`;
        const now = Date.now();
        const lastDrop = dropDedupRef.current.get(dedupKey);
        if (lastDrop && now - lastDrop < 1000) return;
        dropDedupRef.current.set(dedupKey, now);
        for (const [key, timestamp] of dropDedupRef.current) {
          if (now - timestamp >= 1000) dropDedupRef.current.delete(key);
        }

        setBusy("files");
        invoke("send_files", { deviceId, paths })
          .catch((error) => {
            if (active) {
              setNotice({ level: "error", message: normalizeError(error) });
            }
          })
          .finally(() => {
            if (active) setBusy(null);
          });
      })
      .then((cleanup) => {
        if (active) {
          unlisten = cleanup;
        } else {
          cleanup();
        }
      })
      .catch((error) => {
        if (active) {
          setNotice({ level: "error", message: normalizeError(error) });
        }
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  const visibleRemoteEntries = useMemo(() => {
    const query = remoteQuery.trim().toLocaleLowerCase();
    const entries =
      directoryPage?.deviceId === selectedPeerId
        ? directoryPage.entries
        : [];
    return entries
      .filter(
        (entry) =>
          !query ||
          entry.name.toLocaleLowerCase().includes(query),
      )
      .slice(0, 200);
  }, [
    directoryPage,
    remoteQuery,
    selectedPeerId,
  ]);

  async function execute(key: string, action: () => Promise<unknown>) {
    setBusy(key);
    try {
      await action();
    } catch (error) {
      setNotice({ level: "error", message: normalizeError(error) });
    } finally {
      setBusy(null);
    }
  }

  async function chooseAndSendFiles() {
    const selected = await open({
      multiple: true,
      directory: false,
      title: "选择要发送的文件",
    });
    const paths = selected
      ? Array.isArray(selected)
        ? selected
        : [selected]
      : [];
    if (paths.length) {
      if (!selectedPeerId) throw new Error("请先选择已连接设备");
      await invoke("send_files", { deviceId: selectedPeerId, paths });
    }
  }

  async function openRemoteDirectory(
    shareId: string,
    relativePath: string,
    offset = 0,
    append = false,
  ) {
    if (!selectedPeerId) throw new Error("请先选择已连接设备");
    const page = await invoke<DirectoryPage>("list_remote_directory", {
      deviceId: selectedPeerId,
      shareId,
      relativePath,
      offset,
    });
    setDirectoryPage((current) =>
      append &&
      current?.deviceId === page.deviceId &&
      current.shareId === page.shareId &&
      current.relativePath === page.relativePath
        ? { ...page, entries: [...current.entries, ...page.entries] }
        : page,
    );
  }

  async function chooseSharedDirectory() {
    const selected = await open({
      multiple: false,
      directory: true,
      title: "选择允许对方浏览的目录",
    });
    if (typeof selected === "string") {
      await invoke("add_shared_directory", { path: selected });
    }
  }

  async function connectManualEndpoint() {
    const endpoint = manualEndpoint.trim();
    if (!endpoint) {
      setNotice({ level: "info", message: "请输入对方 IP 或 IP:端口" });
      return;
    }
    await invoke("connect_by_ip", { endpoint });
  }

  async function copyConnectionAddress(address: string) {
    const endpoint = address.includes(" · ")
      ? address.slice(address.lastIndexOf(" · ") + 3)
      : address;
    await navigator.clipboard.writeText(endpoint);
    setNotice({ level: "success", message: `已复制连接地址 ${endpoint}` });
  }

  async function checkForUpdates(manual: boolean) {
    if (isDevelopmentMock) {
      setUpdater((current) => ({ ...current, phase: "current" }));
      if (manual) {
        setNotice({ level: "success", message: "当前已是最新开发版本" });
      }
      return;
    }

    setUpdater((current) => ({
      ...current,
      phase: "checking",
      availableVersion: null,
      notes: null,
      error: null,
      downloadedBytes: 0,
      totalBytes: null,
    }));
    try {
      const update = await check({ timeout: 15_000 });
      if (!updaterMounted.current) {
        await update?.close();
        return;
      }

      const previous = pendingUpdate.current;
      pendingUpdate.current = update;
      if (previous && previous !== update) {
        await previous.close();
      }

      if (update) {
        setUpdater((current) => ({
          ...current,
          phase: "available",
          currentVersion: update.currentVersion,
          availableVersion: update.version,
          notes: update.body ?? null,
        }));
        setNotice({
          level: "success",
          message: `发现 NearWeave ${update.version}，可在设置中安装`,
        });
      } else {
        setUpdater((current) => ({
          ...current,
          phase: "current",
          availableVersion: null,
          notes: null,
        }));
        if (manual) {
          setNotice({ level: "success", message: "当前已是最新版本" });
        }
      }
    } catch (error) {
      const message = normalizeError(error);
      if (updaterMounted.current) {
        setUpdater((current) => ({
          ...current,
          phase: "error",
          error: message,
        }));
        if (manual) {
          setNotice({
            level: "error",
            message: `检查更新失败：${message}`,
          });
        }
      }
    }
  }

  async function installAvailableUpdate() {
    const update = pendingUpdate.current;
    if (!update) {
      await checkForUpdates(true);
      return;
    }

    let downloadedBytes = 0;
    let totalBytes: number | null = null;
    setUpdater((current) => ({
      ...current,
      phase: "downloading",
      downloadedBytes: 0,
      totalBytes: null,
      error: null,
    }));
    try {
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          totalBytes = event.data.contentLength ?? null;
        } else if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength;
        } else if (event.event === "Finished") {
          setUpdater((current) => ({ ...current, phase: "installing" }));
          return;
        }
        setUpdater((current) => ({
          ...current,
          downloadedBytes,
          totalBytes,
        }));
      });
      setUpdater((current) => ({ ...current, phase: "installing" }));
      await relaunch();
    } catch (error) {
      const message = normalizeError(error);
      setUpdater((current) => ({
        ...current,
        phase: "error",
        error: message,
      }));
      setNotice({
        level: "error",
        message: `安装更新失败：${message}`,
      });
    }
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <img src={nearweaveIcon} alt="" />
          </span>
          <div>
            <strong>NearWeave</strong>
            <span>附近设备互传</span>
          </div>
        </div>

        <ConnectionRail
          snapshot={snapshot}
          selectedConnection={selectedConnection}
        />

        <div className="topbar-actions">
          <button
            className={`settings-button ${
              updater.phase === "available" ? "has-update" : ""
            }`}
            title="打开设置"
            aria-label="打开设置"
            onClick={() => setSettingsOpen(true)}
          >
            <Settings size={17} />
            {updater.phase === "available" && (
              <span className="update-badge" aria-label="发现新版本" />
            )}
          </button>
        </div>
      </header>

      <div className="workspace">
        <aside className="device-panel">
          <section
            className={`listener-card ${serviceRunning ? "active" : ""}`}
          >
            <span
              className={serviceRunning ? "live-dot" : "live-dot off"}
            />
            <div>
              <strong>
                {snapshot.connectionServiceState === "starting"
                  ? "正在恢复连接服务"
                  : snapshot.connectionServiceState === "stopping"
                    ? "正在停止连接服务"
                    : serviceRunning
                      ? `NearWeave 连接运行中 · ${snapshot.connections.length} 台已连接`
                      : "NearWeave 连接已停止"}
              </strong>
              <span>
                {serviceRunning
                  ? "局域网、热点与已配对蓝牙均可连接，最多同时连接 8 台设备"
                  : "恢复后才允许发现、连接和传输；不会自动重连之前设备"}
              </span>
            </div>
            <button
              className="listener-control-button"
              aria-label={serviceRunning ? "停止连接" : "恢复连接"}
              disabled={busy === "listening" || serviceTransitioning}
              onClick={() =>
                execute("listening", () =>
                  invoke("set_connection_service_enabled", {
                    enabled: !serviceRunning,
                  }),
                )
              }
            >
              {serviceTransitioning && (
                <LoaderCircle size={13} className="spin" />
              )}
              {serviceRunning ? "停止连接" : "恢复连接"}
            </button>
          </section>

          <div className="listener-components" aria-label="NearWeave Received服务状态">
            <ServiceBadge
              label="蓝牙"
              status={snapshot.listenerStatus.bluetooth}
              error={snapshot.listenerStatus.bluetoothError}
            />
            <ServiceBadge
              label="局域网发现"
              status={snapshot.listenerStatus.discovery}
              error={snapshot.listenerStatus.discoveryError}
            />
            <ServiceBadge
              label={`TCP ${
                snapshot.listenerStatus.tcpPort
                  ? snapshot.listenerStatus.tcpPort
                  : ""
              }`}
              status={snapshot.listenerStatus.tcp}
              error={snapshot.listenerStatus.tcpError}
            />
            {serviceRunning &&
              snapshot.listenerStatus.localAddresses.map((address) => (
                <button
                  type="button"
                  className="connection-address"
                  key={address}
                  title="复制 IP:端口，用于手动直连"
                  onClick={() => void copyConnectionAddress(address)}
                >
                  <Copy size={11} />
                  {address}
                </button>
              ))}
          </div>

          <div className="panel-heading">
            <div>
              <span className="eyebrow">附近设备</span>
              <h2>选择一台电脑</h2>
            </div>
            <button
              className="icon-button"
              title="重新扫描"
              disabled={!serviceRunning || busy === "scan"}
              onClick={() => execute("scan", () => invoke("refresh_devices"))}
            >
              <RefreshCw
                size={17}
                className={busy === "scan" ? "spin" : undefined}
              />
            </button>
          </div>

          <form
            className="manual-connect"
            onSubmit={(event) => {
              event.preventDefault();
              void execute("manual-connect", connectManualEndpoint);
            }}
          >
            <Wifi size={14} />
            <input
              aria-label="对方 IP 或 IP 端口"
              placeholder="输入 IP 或 IP:端口"
              value={manualEndpoint}
              disabled={!serviceRunning}
              onChange={(event) => setManualEndpoint(event.target.value)}
            />
            <button
              type="submit"
              disabled={
                !serviceRunning ||
                busy === "manual-connect"
              }
            >
              {busy === "manual-connect" ? (
                <LoaderCircle size={13} className="spin" />
              ) : (
                "连接"
              )}
            </button>
          </form>

          <div className="device-list">
            {snapshot.connections.map((connection) => (
              <article
                className={`device-row connected-device ${
                  selectedPeerId === connection.deviceId ? "selected" : ""
                }`}
                key={connection.deviceId}
                role="button"
                tabIndex={0}
                onClick={() => setSelectedPeerId(connection.deviceId)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    setSelectedPeerId(connection.deviceId);
                  }
                }}
              >
                <span className="device-icon">
                  <Laptop size={19} />
                </span>
                <div>
                  <strong>{connection.name}</strong>
                  <span>
                    {connection.connectionKind === "lan"
                      ? "局域网直连"
                      : connection.networkConnected
                        ? "蓝牙 + 局域网高速链路"
                        : "蓝牙连接"}
                  </span>
                </div>
                <span className="connected-device-actions">
                  {selectedPeerId === connection.deviceId && (
                    <span className="connected-check" title="当前发送目标">
                      <Check size={14} />
                    </span>
                  )}
                  <button
                    type="button"
                    className="device-disconnect-button"
                    title={`断开 ${connection.name}`}
                    disabled={busy === `disconnect-${connection.deviceId}`}
                    onClick={(event) => {
                      event.stopPropagation();
                      void execute(`disconnect-${connection.deviceId}`, () =>
                        invoke("disconnect_device", {
                          deviceId: connection.deviceId,
                        }),
                      );
                    }}
                  >
                    {busy === `disconnect-${connection.deviceId}` ? (
                      <LoaderCircle size={13} className="spin" />
                    ) : (
                      <Unlink size={13} />
                    )}
                  </button>
                </span>
              </article>
            ))}

            {snapshot.devices
              .filter(
                (device) =>
                  !snapshot.connections.some(
                    (connection) =>
                      connection.deviceId === (device.deviceId ?? device.id),
                  ),
              )
              .map((device) => (
                <button
                  className={`device-row ${
                    device.lanEndpoint || device.nearweaveEnabled
                      ? "available"
                      : "unavailable"
                  }`}
                  key={device.id}
                  disabled={
                    !serviceRunning ||
                    (!device.lanEndpoint && !device.nearweaveEnabled) ||
                    busy === `connect-${device.id}`
                  }
                  onClick={() =>
                    execute(`connect-${device.id}`, () =>
                      invoke("connect_peer", { deviceId: device.id }),
                    )
                  }
                >
                  <span className="device-icon">
                    <Laptop size={19} />
                  </span>
                  <span className="device-copy">
                    <strong>{device.name || "未命名电脑"}</strong>
                    <span className="device-capabilities">
                      {device.lanEndpoint && (
                        <em className="capability lan">局域网</em>
                      )}
                      {device.bluetoothPaired && (
                        <em
                          className={`capability bluetooth ${
                            device.nearweaveEnabled ? "" : "muted"
                          }`}
                        >
                          {device.nearweaveEnabled
                            ? "蓝牙可连接"
                            : "蓝牙已配对"}
                        </em>
                      )}
                      {device.trusted && (
                        <em className="capability trusted">已信任</em>
                      )}
                    </span>
                  </span>
                  {busy === `connect-${device.id}` ? (
                    <LoaderCircle size={16} className="spin" />
                  ) : device.lanEndpoint || device.nearweaveEnabled ? (
                    <ChevronRight size={17} />
                  ) : (
                    <span className="device-status-dot" aria-hidden="true" />
                  )}
                </button>
              ))}

            {snapshot.connections.length === 0 &&
              snapshot.devices.length === 0 && (
              <div className="empty-device">
                <Radio size={25} />
                <strong>暂未发现附近的 NearWeave 设备</strong>
                <span>
                  可让对方恢复 NearWeave 连接并保持同一局域网或热点网络，或先完成蓝牙配对。
                </span>
                <button
                  className="text-button"
                  disabled={!serviceRunning}
                  onClick={() =>
                    execute("scan", () => invoke("refresh_devices"))
                  }
                >
                  立即扫描
                </button>
              </div>
            )}
          </div>

          <div className="trust-note">
            <ShieldCheck size={17} />
            <p>
              <strong>首次局域网连接需要双方核对验证码</strong>
              确认后会记住设备身份；蓝牙连接继续使用 Windows 配对与加密。
            </p>
          </div>

        </aside>

        <section className="dashboard">
          <div className="action-strip">
            <ActionCard
              accent="blue"
              icon={<Send size={21} />}
              label="发送文件"
              detail="选择文件或直接拖入窗口"
              disabled={!serviceRunning || !selectedPeerId || busy === "files"}
              loading={busy === "files"}
              onClick={() => execute("files", chooseAndSendFiles)}
            />
            <ActionCard
              accent="teal"
              icon={<FolderUp size={21} />}
              label="共享目录"
              detail="对方可浏览并按需下载"
              disabled={!serviceRunning || busy === "share"}
              loading={busy === "share"}
              onClick={() => execute("share", chooseSharedDirectory)}
            />
            <div
              className={`action-card clipboard-card ${
                snapshot.clipboardEnabled ? "active" : ""
              }`}
            >
              <span className="action-icon amber">
                <ClipboardCopy size={21} />
              </span>
              <div>
                <strong>文本剪贴板</strong>
                <span>
                  {snapshot.clipboardEnabled
                    ? "正在双向同步，下次启动沿用"
                    : "已暂停，下次启动沿用"}
                </span>
              </div>
              <button
                className="switch"
                aria-label="切换文本剪贴板同步"
                aria-pressed={snapshot.clipboardEnabled}
                disabled={busy === "clipboard"}
                onClick={() =>
                  execute("clipboard", () =>
                    invoke("set_clipboard_enabled", {
                      enabled: !snapshot.clipboardEnabled,
                    }),
                  )
                }
              >
                <span />
              </button>
            </div>
          </div>

          <div className="content-grid">
            <section className="transfer-panel">
              <div className="section-title">
                <div>
                  <span className="eyebrow">传输队列</span>
                  <h2>最近任务</h2>
                </div>
                <div className="section-actions">
                  <button
                    className="quiet-button"
                    onClick={() =>
                      execute("open-receive", () =>
                        invoke("open_receive_directory"),
                      )
                    }
                  >
                    <FolderOpen size={15} />
                    接收目录
                  </button>
                  <button
                    className="icon-button subtle"
                    title={
                      snapshot.transfers.some(isTerminalTransfer)
                        ? "清理全部已完成或失败记录"
                        : "没有可清理的记录"
                    }
                    disabled={
                      busy === "clear-transfers" ||
                      !snapshot.transfers.some(isTerminalTransfer)
                    }
                    onClick={() =>
                      execute("clear-transfers", () =>
                        invoke("clear_transfer_history"),
                      )
                    }
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </div>

              <div className="transfer-list">
                {snapshot.transfers.length === 0 ? (
                  <div className="empty-transfer">
                    <Inbox size={30} />
                    <strong>等待第一次传输</strong>
                    <span>连接设备后，选择文件或从共享目录下载。</span>
                  </div>
                ) : (
                  snapshot.transfers.slice(0, 12).map((transfer) => (
                    <TransferRow
                      transfer={transfer}
                      key={transfer.id}
                      removing={busy === `remove-transfer-${transfer.id}`}
                      cancelling={busy === `cancel-transfer-${transfer.id}`}
                      onCancel={() =>
                        execute(`cancel-transfer-${transfer.id}`, () =>
                          invoke("cancel_transfer", {
                            deviceId: transfer.peerDeviceId,
                            transferId: transfer.id,
                          }),
                        )
                      }
                      onRemove={() =>
                        execute(`remove-transfer-${transfer.id}`, () =>
                          invoke("remove_transfer", {
                            transferId: transfer.id,
                          }),
                        )
                      }
                    />
                  ))
                )}
              </div>
              <footer className="receive-path" title={snapshot.receiveDirectory}>
                <span>自动接收至</span>
                <code>{snapshot.receiveDirectory || "正在准备目录…"}</code>
              </footer>
              {snapshot.legacyReceiveDirectory && (
                <footer
                  className="receive-path legacy-receive-path"
                  title={snapshot.legacyReceiveDirectory}
                >
                  <span>旧版接收文件仍保留在原目录</span>
                  <button
                    className="text-button"
                    onClick={() =>
                      execute("open-legacy-receive", () =>
                        invoke("open_legacy_receive_directory"),
                      )
                    }
                  >
                    打开原目录
                  </button>
                </footer>
              )}
            </section>

            <aside className="share-panel">
              <div className="share-section remote-section">
                <div className="section-title compact">
                  <div>
                    <span className="eyebrow">对方共享</span>
                    <h2>
                      {selectedConnection
                        ? `${selectedConnection.name} 的目录`
                        : "按需浏览目录"}
                    </h2>
                  </div>
                  <button
                    className="icon-button subtle"
                    title="刷新共享目录"
                    disabled={
                      !serviceRunning || !selectedPeerId || busy === "remote"
                    }
                    onClick={() => {
                      setDirectoryPage(null);
                      void execute("remote", () =>
                        invoke("refresh_remote_shares", {
                          deviceId: selectedPeerId,
                        }),
                      );
                    }}
                  >
                    <RefreshCw
                      size={15}
                      className={busy === "remote" ? "spin" : undefined}
                    />
                  </button>
                </div>

                {selectedPeerId && directoryPage && (
                  <label className="search-box">
                    <Search size={14} />
                    <input
                      value={remoteQuery}
                      onChange={(event) => setRemoteQuery(event.target.value)}
                      placeholder="筛选当前已加载目录"
                    />
                    {remoteQuery && (
                      <button
                        aria-label="清空搜索"
                        onClick={() => setRemoteQuery("")}
                      >
                        <X size={13} />
                      </button>
                    )}
                  </label>
                )}

                <div className="remote-file-list">
                  {!directoryPage &&
                    selectedWorkspace?.roots.map((root) => (
                      <button
                        type="button"
                        className="remote-file remote-directory"
                        key={root.id}
                        disabled={busy === `directory-${root.id}`}
                        onClick={() =>
                          execute(`directory-${root.id}`, () =>
                            openRemoteDirectory(root.id, ""),
                          )
                        }
                      >
                        <span className="file-icon">
                          <Folder size={16} />
                        </span>
                        <div>
                          <strong>{root.name}</strong>
                          <span>共享根目录 · 点击浏览</span>
                        </div>
                        {busy === `directory-${root.id}` ? (
                          <LoaderCircle size={15} className="spin" />
                        ) : (
                          <ChevronRight size={15} />
                        )}
                      </button>
                    ))}

                  {directoryPage && selectedRoot && (
                    <nav className="directory-breadcrumbs" aria-label="目录路径">
                      <button
                        type="button"
                        onClick={() =>
                          execute("directory-root", () =>
                            openRemoteDirectory(selectedRoot.id, ""),
                          )
                        }
                      >
                        {selectedRoot.name}
                      </button>
                      {directorySegments.map((segment, index) => {
                        const path = directorySegments
                          .slice(0, index + 1)
                          .join("/");
                        return (
                          <span key={path}>
                            <ChevronRight size={12} />
                            <button
                              type="button"
                              onClick={() =>
                                execute(`directory-${path}`, () =>
                                  openRemoteDirectory(selectedRoot.id, path),
                                )
                              }
                            >
                              {segment}
                            </button>
                          </span>
                        );
                      })}
                    </nav>
                  )}

                  {visibleRemoteEntries.map((entry) => (
                    <div
                      className={`remote-file ${
                        entry.isDirectory ? "remote-directory" : ""
                      }`}
                      key={`${entry.shareId}:${entry.relativePath}`}
                    >
                      <span className="file-icon">
                        {entry.isDirectory ? (
                          <Folder size={16} />
                        ) : (
                          <File size={16} />
                        )}
                      </span>
                      <div title={`${entry.shareName}/${entry.relativePath}`}>
                        <strong>{entry.name}</strong>
                        <span>
                          {entry.isDirectory
                            ? "文件夹 · 点击后加载"
                            : formatBytes(entry.size)}
                        </span>
                      </div>
                      {entry.isDirectory ? (
                        <button
                          title="打开文件夹"
                          disabled={busy === `directory-${entry.relativePath}`}
                          onClick={() =>
                            execute(`directory-${entry.relativePath}`, () =>
                              openRemoteDirectory(
                                entry.shareId,
                                entry.relativePath,
                              ),
                            )
                          }
                        >
                          <ChevronRight size={15} />
                        </button>
                      ) : (
                        <button
                          title="下载文件"
                          disabled={
                            busy === `download-${entry.relativePath}` ||
                            !selectedPeerId
                          }
                          onClick={() =>
                            execute(`download-${entry.relativePath}`, () =>
                              invoke("download_shared_file", {
                                deviceId: selectedPeerId,
                                shareId: entry.shareId,
                                relativePath: entry.relativePath,
                              }),
                            )
                          }
                        >
                          <Download size={15} />
                        </button>
                      )}
                    </div>
                  ))}

                  {directoryPage?.nextOffset !== null &&
                    directoryPage?.nextOffset !== undefined && (
                      <button
                        type="button"
                        className="load-more-button"
                        disabled={busy === "directory-more"}
                        onClick={() =>
                          execute("directory-more", () =>
                            openRemoteDirectory(
                              directoryPage.shareId,
                              directoryPage.relativePath,
                              directoryPage.nextOffset ?? 0,
                              true,
                            ),
                          )
                        }
                      >
                        {busy === "directory-more" && (
                          <LoaderCircle size={13} className="spin" />
                        )}
                        加载更多
                      </button>
                    )}

                  {(!selectedPeerId ||
                    (!directoryPage &&
                      (selectedWorkspace?.roots.length ?? 0) === 0) ||
                    (directoryPage && visibleRemoteEntries.length === 0)) && (
                    <div className="small-empty">
                      <Folder size={23} />
                      <span>
                        {!selectedPeerId
                          ? "连接并选择设备后显示对方授权目录"
                          : directoryPage
                            ? "当前目录没有可显示的条目"
                            : "对方暂未共享目录"}
                      </span>
                    </div>
                  )}
                </div>
              </div>

              <div className="share-section local-section">
                <div className="section-title compact">
                  <div>
                    <span className="eyebrow">本机授权</span>
                    <h2>共享中的目录</h2>
                  </div>
                  <button
                    className="icon-button subtle"
                    title="添加共享目录"
                    disabled={!serviceRunning}
                    onClick={() => execute("share", chooseSharedDirectory)}
                  >
                    <FolderUp size={15} />
                  </button>
                </div>
                <div className="local-share-list">
                  {snapshot.localShares.map((share) => (
                    <div className="local-share" key={share.id}>
                      <Folder size={16} />
                      <div title={share.path}>
                        <strong>{share.name}</strong>
                        <span>{share.path}</span>
                      </div>
                      <button
                        title="停止共享"
                        disabled={busy === `remove-${share.id}`}
                        onClick={() =>
                          execute(`remove-${share.id}`, () =>
                            invoke("remove_shared_directory", {
                              shareId: share.id,
                            }),
                          )
                        }
                      >
                        <X size={14} />
                      </button>
                    </div>
                  ))}
                  {snapshot.localShares.length === 0 && (
                    <div className="small-empty local">
                      <span>尚未授权任何目录</span>
                    </div>
                  )}
                </div>
              </div>
            </aside>
          </div>
          <div
            className={`file-drop-overlay ${fileDropActive ? "visible" : ""}`}
            aria-hidden={!fileDropActive}
          >
            <Send size={34} />
            <strong>
              {!serviceRunning
                ? "连接服务已停止"
                : selectedPeerId
                  ? `松开发送给 ${selectedConnection?.name ?? "当前设备"}`
                  : "请先选择已连接设备"}
            </strong>
            <span>支持一次拖入多个普通文件</span>
          </div>
        </section>
      </div>

      {settingsOpen && (
        <div
          className="settings-backdrop"
          role="presentation"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) setSettingsOpen(false);
          }}
        >
          <section
            className="settings-view"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-title"
          >
            <header>
              <div>
                <span className="eyebrow">应用偏好</span>
                <h2 id="settings-title">设置</h2>
              </div>
              <button
                className="icon-button"
                aria-label="关闭设置"
                onClick={() => setSettingsOpen(false)}
              >
                <X size={17} />
              </button>
            </header>

            <div className="settings-list">
              <article className="setting-row">
                <div>
                  <strong>开机自启动</strong>
                  <span>登录 Windows 后自动运行 NearWeave</span>
                </div>
                <button
                  className="switch"
                  aria-label="切换开机自启动"
                  aria-pressed={snapshot.autostartEnabled}
                  disabled={busy === "autostart"}
                  onClick={() =>
                    execute("autostart", () =>
                      invoke("set_autostart_enabled", {
                        enabled: !snapshot.autostartEnabled,
                      }),
                    )
                  }
                >
                  <span />
                </button>
              </article>

              <article className="setting-row link-policy">
                <div>
                  <strong>传输链路</strong>
                  <span>
                    自动优先使用加密局域网，断开时立即回退到蓝牙
                  </span>
                </div>
                <span
                  className={`link-status ${snapshot.activeLink}`}
                  aria-label={`当前链路：${linkLabel(snapshot.activeLink)}`}
                >
                  {snapshot.activeLink === "network" ? (
                    <Wifi size={15} />
                  ) : (
                    <Bluetooth size={15} />
                  )}
                  {linkLabel(snapshot.activeLink)}
                </span>
              </article>

              <article className="setting-row trusted-setting">
                <div className="trusted-setting-header">
                  <div>
                    <strong>已信任设备</strong>
                    <span>纯局域网再次连接时会自动校验设备身份</span>
                  </div>
                  <span className="trusted-count">
                    {snapshot.trustedDevices.length}
                  </span>
                </div>
                {snapshot.trustedDevices.length > 0 ? (
                  <div className="trusted-device-list">
                    {snapshot.trustedDevices.map((device) => (
                      <div className="trusted-device" key={device.deviceId}>
                        <ShieldCheck size={15} />
                        <div>
                          <strong>{device.name}</strong>
                          <span title={device.fingerprint}>
                            指纹 {shortFingerprint(device.fingerprint)} ·{" "}
                            {formatLastSeen(device.lastSeenAt)}
                          </span>
                        </div>
                        <button
                          type="button"
                          aria-label={`移除对 ${device.name} 的信任`}
                          title="移除信任"
                          disabled={busy === `trust-${device.deviceId}`}
                          onClick={() => {
                            if (
                              window.confirm(
                                `移除对“${device.name}”的信任？下次连接需要重新核对验证码。`,
                              )
                            ) {
                              void execute(`trust-${device.deviceId}`, () =>
                                invoke("remove_trusted_device", {
                                  deviceId: device.deviceId,
                                }),
                              );
                            }
                          }}
                        >
                          {busy === `trust-${device.deviceId}` ? (
                            <LoaderCircle size={13} className="spin" />
                          ) : (
                            <Trash2 size={13} />
                          )}
                        </button>
                      </div>
                    ))}
                  </div>
                ) : (
                  <span className="trusted-empty">
                    尚未信任设备，首次局域网连接后会显示在这里。
                  </span>
                )}
              </article>

              <article className="setting-row updater-setting">
                <div>
                  <div className="setting-title-row">
                    <strong>软件更新</strong>
                    <span
                      className="current-version-badge"
                      aria-label={`当前版本 ${currentVersionLabel(updater.currentVersion)}`}
                    >
                      {currentVersionLabel(updater.currentVersion)}
                    </span>
                  </div>
                  <span title={updater.notes ?? updater.error ?? undefined}>
                    {updateStatusText(updater)}
                  </span>
                  {updater.phase === "downloading" && (
                    <div
                      className="update-progress"
                      role="progressbar"
                      aria-label="更新下载进度"
                      aria-valuemin={0}
                      aria-valuemax={updater.totalBytes ?? undefined}
                      aria-valuenow={updater.downloadedBytes}
                    >
                      <span
                        className={
                          updater.totalBytes === null ? "indeterminate" : ""
                        }
                        style={{
                          width: `${updateProgressPercent(updater)}%`,
                        }}
                      />
                    </div>
                  )}
                </div>
                <button
                  className="update-button"
                  disabled={
                    updater.phase === "checking" ||
                    updater.phase === "downloading" ||
                    updater.phase === "installing"
                  }
                  onClick={() =>
                    updater.availableVersion
                      ? void installAvailableUpdate()
                      : void checkForUpdates(true)
                  }
                >
                  {updater.phase === "checking" ||
                  updater.phase === "downloading" ||
                  updater.phase === "installing" ? (
                    <LoaderCircle size={14} className="spin" />
                  ) : updater.availableVersion ? (
                    <Download size={14} />
                  ) : (
                    <RefreshCw size={14} />
                  )}
                  {updateActionLabel(updater)}
                </button>
              </article>

            </div>

            <footer>
              <span>设置保存在当前 Windows 用户下。</span>
            </footer>
          </section>
        </div>
      )}

      {activePairing && (
        <div className="pairing-backdrop">
          <section
            className="pairing-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="pairing-title"
          >
            <span className="pairing-icon">
              <ShieldCheck size={24} />
            </span>
            <div>
              <span className="eyebrow">首次局域网连接</span>
              <h2 id="pairing-title">
                确认 {activePairing.deviceName}
              </h2>
              <p>请确认两台电脑显示的六位验证码完全一致。</p>
            </div>
            <strong
              className="verification-code"
              aria-label={`验证码 ${activePairing.verificationCode}`}
            >
              {activePairing.verificationCode}
            </strong>
            <div className="pairing-actions">
              <button
                type="button"
                className="pairing-reject"
                disabled={busy === "pairing"}
                onClick={() =>
                  execute("pairing", () =>
                    invoke("reject_pairing", {
                      requestId: activePairing.requestId,
                    }),
                  )
                }
              >
                不一致，拒绝
              </button>
              <button
                type="button"
                className="pairing-confirm"
                disabled={busy === "pairing"}
                onClick={() =>
                  execute("pairing", () =>
                    invoke("confirm_pairing", {
                      requestId: activePairing.requestId,
                    }),
                  )
                }
              >
                {busy === "pairing" && (
                  <LoaderCircle size={14} className="spin" />
                )}
                验证码一致
              </button>
            </div>
          </section>
        </div>
      )}

      {notice && (
        <div className={`toast ${notice.level}`} role="status">
          <span className="toast-mark">
            {notice.level === "success" ? (
              <Check size={15} />
            ) : notice.level === "error" ? (
              <X size={15} />
            ) : (
              <Bluetooth size={15} />
            )}
          </span>
          <span>{notice.message}</span>
          <button aria-label="关闭提示" onClick={() => setNotice(null)}>
            <X size={14} />
          </button>
        </div>
      )}
    </main>
  );
}

function ConnectionRail({
  snapshot,
  selectedConnection,
}: {
  snapshot: AppSnapshot;
  selectedConnection: PeerConnection | null;
}) {
  const connected = snapshot.connections.length > 0;
  return (
    <div
      className={`connection-rail ${connected ? "connected" : ""}`}
      aria-label={
        connected
          ? `已连接 ${snapshot.connections.length} 台设备，当前目标 ${
              selectedConnection?.name ?? "未选择"
            }`
          : "等待设备连接"
      }
    >
      <div className="rail-node local">
        <span>
          <Laptop size={16} />
        </span>
        <div>
          <small>本机</small>
          <strong>{snapshot.deviceName}</strong>
        </div>
      </div>
      <div className="rail-track">
        <i />
        <i />
        <i />
        <span>
          {connected ? (
            <Link2 size={14} />
          ) : snapshot.reconnecting ? (
            <LoaderCircle size={14} className="spin" />
          ) : (
            <Unlink size={14} />
          )}
          {selectedConnection?.connectionKind === "lan"
            ? "局域网直连"
            : selectedConnection?.networkConnected
              ? "局域网优先"
            : connected
              ? "蓝牙已连接"
            : snapshot.reconnecting
              ? "自动重连中"
              : "等待连接"}
        </span>
      </div>
      <div className="rail-node remote">
        <span>
          <Laptop size={16} />
        </span>
        <div>
          <small>
            对端{connected ? ` · ${snapshot.connections.length} 台` : ""}
          </small>
          <strong>{selectedConnection?.name ?? "附近电脑"}</strong>
        </div>
      </div>
    </div>
  );
}

function ServiceBadge({
  label,
  status,
  error,
}: {
  label: string;
  status: ServiceStatus;
  error: string | null;
}) {
  return (
    <span
      className={`service-badge ${status}`}
      title={error ?? `${label}${status === "ready" ? "已就绪" : "未开启"}`}
    >
      <i />
      {label}
    </span>
  );
}

function ActionCard({
  accent,
  icon,
  label,
  detail,
  disabled,
  loading,
  onClick,
}: {
  accent: "blue" | "teal";
  icon: React.ReactNode;
  label: string;
  detail: string;
  disabled: boolean;
  loading: boolean;
  onClick: () => void;
}) {
  return (
    <button className="action-card" disabled={disabled} onClick={onClick}>
      <span className={`action-icon ${accent}`}>
        {loading ? <LoaderCircle size={21} className="spin" /> : icon}
      </span>
      <span className="action-copy">
        <strong>{label}</strong>
        <span>{detail}</span>
      </span>
      <ChevronRight size={17} className="action-chevron" />
    </button>
  );
}

function TransferRow({
  transfer,
  removing,
  cancelling,
  onCancel,
  onRemove,
}: {
  transfer: Transfer;
  removing: boolean;
  cancelling: boolean;
  onCancel: () => void;
  onRemove: () => void;
}) {
  const progress =
    transfer.totalBytes > 0
      ? Math.min(100, (transfer.bytesDone / transfer.totalBytes) * 100)
      : transfer.state === "completed"
        ? 100
        : 0;
  const stateText: Record<TransferState, string> = {
    queued: "排队中",
    transferring: transfer.direction === "sending" ? "发送中" : "接收中",
    waiting_for_peer: "等待校验",
    cancelling: "正在取消",
    canceled: "已取消",
    completed: "已完成",
    failed: "失败",
  };

  return (
    <article
      className={`transfer-row ${transfer.state} ${transfer.direction}`}
    >
      <span className="transfer-direction">
        {transfer.direction === "sending" ? (
          <Send size={17} />
        ) : (
          <Download size={17} />
        )}
      </span>
      <div className="transfer-body">
        <div className="transfer-copy">
          <strong title={transfer.name}>{transfer.name}</strong>
          <span>
            {transfer.peerName} · {transfer.detail}
          </span>
        </div>
        <div
          className="progress-track"
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(progress)}
        >
          <span style={{ width: `${progress}%` }} />
        </div>
        <div className="transfer-stats">
          <span>速率 {formatTransferRate(transfer.bytesPerSecond)}</span>
          <span>已用 {formatDuration(transfer.elapsedMillis)}</span>
          <span>
            预估剩余{" "}
            {transfer.state === "completed"
              ? "0 秒"
              : transfer.estimatedRemainingSeconds === null
                ? "—"
                : formatDuration(transfer.estimatedRemainingSeconds * 1000)}
          </span>
        </div>
      </div>
      <div className="transfer-meta">
        <strong>{stateText[transfer.state]}</strong>
        <span>
          {formatBytes(transfer.bytesDone)} / {formatBytes(transfer.totalBytes)}
        </span>
      </div>
      <div className="transfer-actions">
        {isTerminalTransfer(transfer) ? (
          <button
            className="transfer-remove"
            aria-label={`删除传输记录：${transfer.name}`}
            title="删除此条记录"
            disabled={removing}
            onClick={onRemove}
          >
            {removing ? (
              <LoaderCircle size={14} className="spin" />
            ) : (
              <Trash2 size={14} />
            )}
          </button>
        ) : (
          <button
            className="transfer-remove"
            aria-label={`取消传输：${transfer.name}`}
            title="中断此任务"
            disabled={cancelling || transfer.state === "cancelling"}
            onClick={onCancel}
          >
            {cancelling || transfer.state === "cancelling" ? (
              <LoaderCircle size={14} className="spin" />
            ) : (
              <X size={14} />
            )}
          </button>
        )}
      </div>
    </article>
  );
}

function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB"];
  const index = Math.min(
    units.length - 1,
    Math.floor(Math.log(value) / Math.log(1024)),
  );
  const amount = value / 1024 ** index;
  return `${amount >= 10 || index === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[index]}`;
}

function shortFingerprint(value: string) {
  return value.length > 16
    ? `${value.slice(0, 8)}…${value.slice(-8)}`
    : value;
}

function formatLastSeen(timestamp: number) {
  if (!timestamp) return "尚未连接";
  return `上次连接 ${new Date(timestamp * 1000).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  })}`;
}

function formatTransferRate(bytesPerSecond: number) {
  return bytesPerSecond > 0 ? `${formatBytes(bytesPerSecond)}/秒` : "—";
}

function formatDuration(milliseconds: number) {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return "0 秒";
  if (milliseconds < 1000) return "< 1 秒";
  const totalSeconds = Math.floor(milliseconds / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours} 时 ${minutes} 分`;
  if (minutes > 0) return `${minutes} 分 ${seconds} 秒`;
  return `${seconds} 秒`;
}

function isTerminalTransfer(transfer: Transfer) {
  return (
    transfer.state === "completed" ||
    transfer.state === "failed" ||
    transfer.state === "canceled"
  );
}

function updateStatusText(update: UpdateSnapshot) {
  if (update.phase === "checking") {
    return "正在检查更新";
  }
  if (update.phase === "available") {
    return `发现新版本 v${update.availableVersion}`;
  }
  if (update.phase === "downloading") {
    const total = update.totalBytes
      ? ` / ${formatBytes(update.totalBytes)}`
      : "";
    return `正在下载 ${formatBytes(update.downloadedBytes)}${total}`;
  }
  if (update.phase === "installing") {
    return "更新已下载，正在安装，应用将自动重启";
  }
  if (update.phase === "error") {
    return `${update.availableVersion ? "更新" : "检查"}失败：${update.error ?? "请稍后重试"}`;
  }
  if (update.phase === "current") {
    return "当前版本已是最新版本";
  }
  return "启动后自动检查更新";
}

function currentVersionLabel(version: string) {
  return version ? `v${version}` : "读取中";
}

function updateActionLabel(update: UpdateSnapshot) {
  if (update.phase === "checking") return "正在检查";
  if (update.phase === "downloading") return "正在下载";
  if (update.phase === "installing") return "正在安装";
  if (update.availableVersion) {
    return update.phase === "error"
      ? "重试安装"
      : `安装 v${update.availableVersion}`;
  }
  return update.phase === "idle" ? "检查更新" : "重新检查";
}

function updateProgressPercent(update: UpdateSnapshot) {
  if (!update.totalBytes || update.totalBytes <= 0) return 18;
  return Math.min(
    100,
    Math.max(0, (update.downloadedBytes / update.totalBytes) * 100),
  );
}

function normalizeError(error: unknown) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "操作失败，请检查蓝牙状态后重试";
}

function linkLabel(link: AppSnapshot["activeLink"]) {
  if (link === "network") return "局域网";
  if (link === "bluetooth") return "蓝牙";
  return "未连接";
}

export default App;
