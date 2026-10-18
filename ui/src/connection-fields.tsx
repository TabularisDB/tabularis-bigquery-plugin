import { defineSlot } from "@tabularis/plugin-api";
import type { TypedSlotProps } from "@tabularis/plugin-api";
import type { ChangeEvent, PropsWithChildren } from "react";

import { PLUGIN_ID, styles } from "./styles";

type SecretFieldState = {
  value: string;
  hasStoredValue: boolean;
  dirty: boolean;
};

// These optional fields are available in Tabularis hosts that support
// plugin-owned keychain secrets. Keeping them optional preserves compatibility
// with older hosts and with plugin-api 0.1.x.
type ConnectionContext = TypedSlotProps<"connection-modal.extra_fields">["context"] & {
  extra?: Record<string, string>;
  setExtraField?: (key: string, value: string) => void;
  secretFields?: Record<string, SecretFieldState>;
  setSecretField?: (key: string, value: string) => void;
};

type AuthMethod = "adc" | "service_account" | "authorized_user" | "access_token";

function Field({ label, children }: PropsWithChildren<{ label: string }>) {
  return (
    <label style={styles.field}>
      <span style={styles.label}>{label}</span>
      {children}
    </label>
  );
}

function credentialLabel(authMethod: AuthMethod): string {
  if (authMethod === "access_token") return "OAuth access token";
  if (authMethod === "authorized_user") return "OAuth authorized user JSON or file path";
  return "Service account JSON or file path";
}

function authenticationHelp(authMethod: AuthMethod, target: string): string {
  if (authMethod === "service_account") {
    return `Put the service account JSON file path or the complete JSON key in ${target}.`;
  }
  if (authMethod === "authorized_user") {
    return `Put an authorized_user credential JSON file path or complete JSON in ${target}. The plugin refreshes its access token automatically.`;
  }
  if (authMethod === "access_token") {
    return `Put a current OAuth access token in ${target}. Access tokens expire and must be replaced manually.`;
  }
  return "No credential field is used. The plugin follows Application Default Credentials, including GOOGLE_APPLICATION_CREDENTIALS, gcloud ADC and attached service accounts.";
}

const BigQueryConnectionFields = defineSlot(
  "connection-modal.extra_fields",
  ({ context }) => {
    const connection = context as ConnectionContext;
    const setExtraField = connection.setExtraField;

    if (connection.driver !== PLUGIN_ID || !setExtraField) return null;

    const extra = connection.extra ?? {};
    const authMethod = (extra.auth_method || "adc") as AuthMethod;
    const setSecretField = connection.setSecretField;
    const secureCredentialAvailable = typeof setSecretField === "function";
    const credential = connection.secretFields?.credential ?? {
      value: "",
      hasStoredValue: false,
      dirty: false,
    };
    const target = secureCredentialAvailable ? "Credential" : "Password";
    const placeholder =
      credential.hasStoredValue && !credential.dirty
        ? "Stored securely. Enter a value only to replace it."
        : authMethod === "access_token"
          ? "ya29..."
          : "Paste JSON or enter an absolute file path";
    const status = credential.dirty
      ? credential.value
        ? "The new credential will be stored securely when you save."
        : "The stored credential will be cleared when you save."
      : credential.hasStoredValue
        ? "A credential is stored securely for this connection."
        : "The credential will be stored in the OS keychain.";

    return (
      <div style={styles.container}>
        <div style={styles.notice}>
          <strong>BigQuery connection mapping: </strong>
          {secureCredentialAvailable
            ? "GCP Project ID and authentication are the only required connection fields. Credentials are stored in the OS keychain."
            : "Database is the GCP Project ID. Host, Port and Username are ignored. Password contains the selected credential when required."}
        </div>

        <Field label="Authentication">
          <select
            value={authMethod}
            onChange={(event: ChangeEvent<HTMLSelectElement>) =>
              setExtraField("auth_method", event.target.value)
            }
            style={styles.input}
          >
            <option value="adc">Application Default Credentials</option>
            <option value="service_account">Service account JSON</option>
            <option value="authorized_user">OAuth authorized user JSON</option>
            <option value="access_token">OAuth access token</option>
          </select>
        </Field>

        <p style={styles.help}>{authenticationHelp(authMethod, target)}</p>

        {authMethod !== "adc" && secureCredentialAvailable ? (
          <Field label={credentialLabel(authMethod)}>
            {authMethod === "access_token" ? (
              <input
                type="password"
                value={credential.value}
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  setSecretField("credential", event.target.value)
                }
                placeholder={placeholder}
                autoComplete="off"
                spellCheck={false}
                style={styles.input}
              />
            ) : (
              <textarea
                value={credential.value}
                onChange={(event: ChangeEvent<HTMLTextAreaElement>) =>
                  setSecretField("credential", event.target.value)
                }
                placeholder={placeholder}
                autoComplete="off"
                spellCheck={false}
                rows={4}
                style={{ ...styles.input, ...styles.textarea }}
              />
            )}
            <span style={styles.status}>{status}</span>
            {credential.hasStoredValue && !credential.dirty ? (
              <button
                type="button"
                onClick={() => setSecretField("credential", "")}
                style={styles.clearButton}
              >
                Clear stored credential
              </button>
            ) : null}
          </Field>
        ) : null}

        <div style={styles.grid}>
          <Field label="Location">
            <input
              value={extra.location || ""}
              onChange={(event: ChangeEvent<HTMLInputElement>) =>
                setExtraField("location", event.target.value)
              }
              placeholder="US, EU, europe-west1"
              autoComplete="off"
              spellCheck={false}
              style={styles.input}
            />
          </Field>
          <Field label="Query timeout, seconds">
            <input
              type="number"
              min={30}
              max={3600}
              value={extra.query_timeout_seconds || "300"}
              onChange={(event: ChangeEvent<HTMLInputElement>) =>
                setExtraField("query_timeout_seconds", event.target.value)
              }
              style={styles.input}
            />
          </Field>
        </div>

        <Field label="Maximum bytes billed per query">
          <input
            type="number"
            min={0}
            value={extra.max_bytes_billed || ""}
            onChange={(event: ChangeEvent<HTMLInputElement>) =>
              setExtraField("max_bytes_billed", event.target.value)
            }
            placeholder="Optional, for example 1000000000"
            style={styles.input}
          />
        </Field>
      </div>
    );
  },
);

export default BigQueryConnectionFields.component;
