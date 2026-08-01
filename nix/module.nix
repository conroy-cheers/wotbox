{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.wotbox;
  inherit (lib) mkEnableOption mkIf mkOption;
  toml = pkgs.formats.toml { };

  trackerType = lib.types.submodule {
    options = {
      kind = mkOption {
        type = lib.types.enum [
          "ops"
          "red"
        ];
      };
      baseUrl = mkOption { type = lib.types.str; };
      tokenFile = mkOption { type = lib.types.str; };
      announceHosts = mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
      };
    };
  };

  clientType = lib.types.submodule {
    options = {
      baseUrl = mkOption { type = lib.types.str; };
      apiKeyFile = mkOption { type = lib.types.str; };
    };
  };

  profileType = lib.types.submodule {
    options = {
      client = mkOption { type = lib.types.str; };
      savePath = mkOption { type = lib.types.str; };
      tag = mkOption { type = lib.types.str; };
      startPaused = mkOption {
        type = lib.types.bool;
        default = false;
      };
    };
  };

  plexType = lib.types.submodule {
    options = {
      baseUrl = mkOption {
        type = lib.types.str;
        default = "http://127.0.0.1:32400";
      };
      tokenFile = mkOption { type = lib.types.str; };
      sectionId = mkOption { type = lib.types.ints.positive; };
      libraryRoots = mkOption {
        type = lib.types.nonEmptyListOf lib.types.str;
        description = "Absolute music library paths accepted for Plex partial scans.";
      };
    };
  };

  settings = {
    listen_address = cfg.listenAddress;
    inherit (cfg) port;
    base_path = cfg.basePath;
    database_path = "${cfg.stateDirectory}/wotbox.sqlite";
    trackers = lib.mapAttrs (_: tracker: {
      inherit (tracker) kind;
      base_url = tracker.baseUrl;
      token_file = tracker.tokenFile;
      announce_hosts = tracker.announceHosts;
    }) cfg.trackers;
    download_clients = lib.mapAttrs (_: client: {
      kind = "qbittorrent";
      base_url = client.baseUrl;
      api_key_file = client.apiKeyFile;
    }) cfg.downloadClients;
    download_profiles = lib.mapAttrs (_: profile: {
      inherit (profile) client tag;
      save_path = profile.savePath;
      start_paused = profile.startPaused;
    }) cfg.downloadProfiles;
  }
  // lib.optionalAttrs (cfg.lastfmApiKeyFile != null) {
    lastfm_api_key_file = cfg.lastfmApiKeyFile;
  }
  // lib.optionalAttrs (cfg.plex != null) {
    plex = {
      base_url = cfg.plex.baseUrl;
      token_file = cfg.plex.tokenFile;
      section_id = cfg.plex.sectionId;
      library_roots = cfg.plex.libraryRoots;
    };
  };
  configFile = toml.generate "wotbox.toml" settings;
in
{
  options.services.wotbox = {
    enable = mkEnableOption "Wotbox";
    package = mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.default;
      defaultText = lib.literalExpression "inputs.wotbox.packages.\${pkgs.system}.default";
    };
    user = mkOption {
      type = lib.types.str;
      default = "wotbox";
    };
    group = mkOption {
      type = lib.types.str;
      default = "wotbox";
    };
    listenAddress = mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
    };
    port = mkOption {
      type = lib.types.port;
      default = 8780;
    };
    basePath = mkOption {
      type = lib.types.str;
      default = "/";
    };
    stateDirectory = mkOption {
      type = lib.types.str;
      default = "/var/lib/wotbox";
      readOnly = true;
    };
    workerThreads = mkOption {
      type = lib.types.ints.positive;
      default = 4;
      description = "Number of Tokio runtime worker threads used by Wotbox";
    };
    trackers = mkOption {
      type = lib.types.attrsOf trackerType;
      default = { };
    };
    downloadClients = mkOption {
      type = lib.types.attrsOf clientType;
      default = { };
    };
    downloadProfiles = mkOption {
      type = lib.types.attrsOf profileType;
      default = { };
    };
    lastfmApiKeyFile = mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Path to the Last.fm API key used by the optional discovery channel.";
    };
    plex = mkOption {
      type = lib.types.nullOr plexType;
      default = null;
      description = "Optional Plex server notified when music downloads complete.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.trackers != { };
        message = "services.wotbox.trackers must contain at least one tracker";
      }
      {
        assertion = cfg.downloadClients != { };
        message = "services.wotbox.downloadClients must contain at least one client";
      }
    ];

    users.users.${cfg.user} = {
      isSystemUser = true;
      inherit (cfg) group;
    };
    users.groups.${cfg.group} = { };

    systemd.services.wotbox = {
      description = "Wotbox tracker manager";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      restartTriggers = [ configFile ];
      environment = {
        RUST_LOG = "wotbox=info,tower_http=info";
        TOKIO_WORKER_THREADS = toString cfg.workerThreads;
      };
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
        Restart = "on-failure";
        RestartSec = "5s";
        User = cfg.user;
        Group = cfg.group;
        StateDirectory = "wotbox";
        WorkingDirectory = cfg.stateDirectory;
        UMask = "0077";

        CapabilityBoundingSet = "";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        ReadWritePaths = [ cfg.stateDirectory ];
        RemoveIPC = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
      };
    };
  };
}
