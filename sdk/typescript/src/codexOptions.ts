export type CodexConfigValue = string | number | boolean | CodexConfigValue[] | CodexConfigObject;

export type CodexConfigObject = { [key: string]: CodexConfigValue };

export type CodexOptions = {
  codexPathOverride?: string;
  /**
   * Experimental: attach exec to an existing local shared app-server socket.
   * Requires a compatible fork CLI. The server owns credentials/environment.
   * Supported on Unix only; connection loss does not guarantee cancellation.
   */
  appServerSocket?: string;
  baseUrl?: string;
  apiKey?: string;
  /**
   * Additional `--config key=value` overrides to pass to the Codex CLI.
   *
   * Provide a JSON object and the SDK will flatten it into dotted paths and
   * serialize values as TOML literals so they are compatible with the CLI's
   * `--config` parsing.
   */
  config?: CodexConfigObject;
  /**
   * Raw `--config key=value` overrides to pass unchanged to the Codex CLI after
   * structured configuration and before SDK-managed or thread-specific overrides.
   */
  configOverrides?: string[];
  /**
   * Environment variables passed to the Codex CLI process. When provided, the SDK
   * will not inherit variables from `process.env`.
   */
  env?: Record<string, string>;
};
