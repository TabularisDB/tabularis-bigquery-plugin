var __tabularis_plugin__ = (function (React) {
  "use strict";

  var inputStyle = {
    width: "100%",
    boxSizing: "border-box",
    padding: "8px 12px",
    borderRadius: "6px",
    border: "1px solid var(--color-border-strong, #475569)",
    background: "var(--color-bg-base, transparent)",
    color: "var(--color-text-primary, inherit)",
    fontSize: "13px",
  };

  function Field(props) {
    return React.createElement(
      "label",
      { style: { display: "flex", flexDirection: "column", gap: "4px" } },
      React.createElement(
        "span",
        {
          style: {
            color: "var(--color-text-muted, #94a3b8)",
            fontSize: "10px",
            fontWeight: 600,
            letterSpacing: "0.06em",
            textTransform: "uppercase",
          },
        },
        props.label,
      ),
      props.children,
    );
  }

  function ConnectionFields(props) {
    var context = props.context || {};
    var extra = context.extra || {};
    var setExtraField = context.setExtraField;
    if (context.driver !== "bigquery" || typeof setExtraField !== "function") return null;

    var authMethod = extra.auth_method || "adc";
    var help =
      authMethod === "service_account"
        ? "Put the service account JSON file path or the complete JSON key in Password. Enable keychain storage when using inline JSON."
        : authMethod === "authorized_user"
          ? "Put an authorized_user credential JSON file path or complete JSON in Password. The plugin refreshes its access token automatically."
        : authMethod === "access_token"
          ? "Put a current OAuth access token in Password. Access tokens expire and must be replaced manually."
          : "Password is not used. The plugin follows Application Default Credentials, including GOOGLE_APPLICATION_CREDENTIALS, gcloud ADC and attached service accounts.";

    return React.createElement(
      "div",
      { style: { display: "flex", flexDirection: "column", gap: "12px" } },
      React.createElement(
        "div",
        {
          style: {
            border: "1px solid color-mix(in srgb, #4285f4 35%, transparent)",
            borderRadius: "8px",
            background: "color-mix(in srgb, #4285f4 8%, transparent)",
            color: "var(--color-text-secondary, #cbd5e1)",
            fontSize: "12px",
            lineHeight: 1.5,
            padding: "10px 12px",
          },
        },
        React.createElement("strong", null, "BigQuery connection mapping: "),
        "Database is the GCP Project ID. Host, Port and Username are ignored. Password contains the selected credential when required.",
      ),
      React.createElement(
        Field,
        { label: "Authentication" },
        React.createElement(
          "select",
          {
            value: authMethod,
            onChange: function (event) {
              setExtraField("auth_method", event.target.value);
            },
            style: inputStyle,
          },
          React.createElement("option", { value: "adc" }, "Application Default Credentials"),
          React.createElement("option", { value: "service_account" }, "Service account JSON"),
          React.createElement("option", { value: "authorized_user" }, "OAuth authorized user JSON"),
          React.createElement("option", { value: "access_token" }, "OAuth access token"),
        ),
      ),
      React.createElement(
        "p",
        {
          style: {
            color: "var(--color-text-muted, #94a3b8)",
            fontSize: "11px",
            lineHeight: 1.5,
            margin: "-6px 0 0",
          },
        },
        help,
      ),
      React.createElement(
        "div",
        { style: { display: "grid", gridTemplateColumns: "1fr 1fr", gap: "12px" } },
        React.createElement(
          Field,
          { label: "Location" },
          React.createElement("input", {
            value: extra.location || "",
            onChange: function (event) {
              setExtraField("location", event.target.value);
            },
            placeholder: "US, EU, europe-west1",
            autoComplete: "off",
            spellCheck: false,
            style: inputStyle,
          }),
        ),
        React.createElement(
          Field,
          { label: "Query timeout, seconds" },
          React.createElement("input", {
            type: "number",
            min: 30,
            max: 3600,
            value: extra.query_timeout_seconds || "300",
            onChange: function (event) {
              setExtraField("query_timeout_seconds", event.target.value);
            },
            style: inputStyle,
          }),
        ),
      ),
      React.createElement(
        Field,
        { label: "Maximum bytes billed per query" },
        React.createElement("input", {
          type: "number",
          min: 0,
          value: extra.max_bytes_billed || "",
          onChange: function (event) {
            setExtraField("max_bytes_billed", event.target.value);
          },
          placeholder: "Optional, for example 1000000000",
          style: inputStyle,
        }),
      ),
    );
  }

  return ConnectionFields;
})(React);
