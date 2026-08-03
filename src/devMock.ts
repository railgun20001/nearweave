export async function installDevelopmentMock() {
  const { mockIPC, mockWindows } = await import("@tauri-apps/api/mocks");
  mockWindows("main");
  const showLanSetup = new URLSearchParams(window.location.search).has("lan-setup");
  const snapshot = {
    platform: "windows",
    deviceId: "00000000-0000-0000-0000-000000000001",
    deviceName: "WORKSTATION-A",
    listening: true,
    connectionServiceState: "running",
    listenerStatus: {
      bluetooth: "ready",
      discovery: "ready",
      tcp: "ready",
      bluetoothError: null,
      discoveryError: null,
      tcpError: null,
      tcpPort: 37992,
      localAddresses: ["192.168.1.20:37992"],
    },
    connected: false,
    networkConnected: false,
    activeLink: "none",
    reconnecting: false,
    peerName: null,
    connectionKind: null,
    pairingRequest: null,
    pairingRequests: [],
    connections: [
      {
        deviceId: "00000000-0000-0000-0000-000000000002",
        name: "LAPTOP-STUDIO",
        connectionKind: "bluetooth",
        bluetoothConnected: true,
        networkConnected: true,
        reconnecting: false,
      },
    ],
    trustedDevices: [
      {
        deviceId: "00000000-0000-0000-0000-000000000002",
        name: "LAPTOP-STUDIO",
        fingerprint:
          "6b4b2f39826f420c8b665d38892cf4f4639e02e413836469261708d550018033",
        lastSeenAt: 1785427200,
      },
    ],
    clipboardEnabled: true,
    autostartEnabled: false,
    lanEnabled: !showLanSetup,
    lanSetupRequired: showLanSetup,
    receiveDirectory: "C:\\Users\\Demo\\Downloads\\NearWeave Received",
    legacyReceiveDirectory: "C:\\Users\\Demo\\Downloads\\旧版接收",
    devices: [
      {
        id: "00000000-0000-0000-0000-000000000002",
        deviceId: "00000000-0000-0000-0000-000000000002",
        name: "LAPTOP-STUDIO",
        bluetoothEndpoint: "bluetooth-device-enabled",
        lanEndpoint: "192.168.1.21:37992",
        bluetoothPaired: true,
        nearweaveEnabled: true,
        trusted: true,
      },
      {
        id: "bluetooth-device-disabled",
        deviceId: null,
        name: "OFFICE-PC",
        bluetoothEndpoint: "bluetooth-device-disabled",
        lanEndpoint: null,
        bluetoothPaired: true,
        nearweaveEnabled: false,
        trusted: false,
      },
    ],
    localShares: [
      {
        id: "00000000-0000-0000-0000-000000000010",
        name: "项目交付",
        path: "C:\\Work\\项目交付",
      },
    ],
    remoteWorkspaces: [
      {
        deviceId: "00000000-0000-0000-0000-000000000002",
        roots: [
          {
            id: "00000000-0000-0000-0000-000000000020",
            name: "设计素材",
          },
        ],
      },
    ],
    transfers: [
      {
        id: "00000000-0000-0000-0000-000000000030",
        peerDeviceId: "00000000-0000-0000-0000-000000000002",
        peerName: "LAPTOP-STUDIO",
        name: "产品演示.mp4",
        direction: "receiving",
        state: "transferring",
        bytesDone: 6815744,
        totalBytes: 18874368,
        detail: "正在接收",
        bytesPerSecond: 1572864,
        elapsedMillis: 4300,
        estimatedRemainingSeconds: 8,
      },
      {
        id: "00000000-0000-0000-0000-000000000031",
        peerDeviceId: "00000000-0000-0000-0000-000000000002",
        peerName: "LAPTOP-STUDIO",
        name: "接口说明.pdf",
        direction: "sending",
        state: "completed",
        bytesDone: 782336,
        totalBytes: 782336,
        detail: "接收并校验完成",
        bytesPerSecond: 391168,
        elapsedMillis: 2200,
        estimatedRemainingSeconds: null,
      },
    ],
  };

  mockIPC(
    (command, payload) => {
      if (command === "get_snapshot") return snapshot;
      if (command === "list_remote_directory") {
        const request = (payload ?? {}) as {
          deviceId?: string;
          shareId?: string;
          relativePath?: string;
          offset?: number;
        };
        return {
          deviceId: request.deviceId,
          shareId: request.shareId,
          relativePath: request.relativePath ?? "",
          offset: request.offset ?? 0,
          nextOffset: null,
          entries:
            request.relativePath === "品牌"
              ? [
                  {
                    shareId: request.shareId,
                    shareName: "设计素材",
                    relativePath: "品牌/NearWeave 图标.sketch",
                    name: "NearWeave 图标.sketch",
                    size: 2654208,
                    isDirectory: false,
                  },
                ]
              : [
                  {
                    shareId: request.shareId,
                    shareName: "设计素材",
                    relativePath: "品牌",
                    name: "品牌",
                    size: 0,
                    isDirectory: true,
                  },
                  {
                    shareId: request.shareId,
                    shareName: "设计素材",
                    relativePath: "演示视频.mp4",
                    name: "演示视频.mp4",
                    size: 18874368,
                    isDirectory: false,
                  },
                ],
        };
      }
      return null;
    },
    { shouldMockEvents: true },
  );
}
