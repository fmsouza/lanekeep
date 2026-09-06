import { defineRule } from 'lanekeep'

// Taint-analysis calibration rule for #220 — the re-run of #195 (docs/taint-calibration.md).
//
// The `flow` queries are transcribed verbatim from that document's appendix, fitted to
// perawallet/pera-react-native @ 3b17bb2. The calibration compares this rule's output across
// two lanekeep builds, so the queries must stay byte-for-byte what #195 measured — do not
// edit them without re-pinning the whole measurement.
//
// id uses the `local/` namespace (built-in-exempt) so the harness config and the RuleTester
// test both load it without declaring a namespace. #195 used `calib/`; the prefix is cosmetic
// and changes no finding.
//
// checkFlow encodes the source location and step count into the message because the JSON
// Violation carries only the sink location — the source data reaches machine-readable output
// only if the message carries it (docs/taint-calibration.md, "To reproduce").
export default defineRule({
  id: 'local/no-secret-in-string',
  severity: 'error',
  requires: ['dataflow'],

  card: {
    message: 'A secret value reaches a string.',
    remediation: 'Redact or hash it before it becomes a string.',
    examples: {
      bad: 'log(getSecret())',
      good: 'log(redact(getSecret()))',
    },
  },

  flow: {
    sources: [
      // 1a. withSecret family — parenthesized single-param arrow: (bytes) => ...
      `(call_expression
   function: (identifier) @fn
   (#any-of? @fn "withSecret" "withBackupMnemonic" "withBackupAuthSecretKey" "withBackupEncryptionKey")
   arguments: (arguments (arrow_function
     parameters: (formal_parameters . (required_parameter (identifier) @source)))))`,
      // 1b. withSecret family — unparenthesized single-param arrow: bytes => ...
      `(call_expression
   function: (identifier) @fn
   (#any-of? @fn "withSecret" "withBackupMnemonic" "withBackupAuthSecretKey" "withBackupEncryptionKey")
   arguments: (arguments (arrow_function parameter: (identifier) @source)))`,
      // 2. Direct-return secret accessors.
      `(call_expression function: (identifier) @fn
   (#any-of? @fn "consumePendingImportMnemonic" "entropyToMnemonic" "mnemonicIndexToWord")) @source`,
      // 3. Property reads on secret-shaped fields.
      `(member_expression property: (property_identifier) @prop
   (#any-of? @prop "privateKey" "secretKey" "mnemonic")) @source`,
    ],
    sinks: [
      `(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "logger") (#any-of? @prop "error" "warn" "info" "debug" "critical")
   arguments: (arguments (_) @sink))`,
      `(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "console") (#any-of? @prop "log" "warn" "error" "debug" "info")
   arguments: (arguments (_) @sink))`,
      `(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#eq? @obj "JSON") (#eq? @prop "stringify") arguments: (arguments (_) @sink))`,
      `(template_substitution (_) @sink)`,
      `(call_expression function: (member_expression object: (identifier) @obj property: (property_identifier) @prop)
   (#any-of? @obj "Sentry" "analytics" "analyticsService")
   (#any-of? @prop "captureException" "captureMessage" "logEvent")
   arguments: (arguments (_) @sink))`,
      `(pair key: [(property_identifier) (string)] @key (#any-of? @key "body" "json") value: (_) @sink)`,
    ],
    sanitizers: [
      `(call_expression function: (identifier) @sanitizer
   (#any-of? @sanitizer "redactSensitiveUrl" "redactSensitiveContext" "redactSensitiveValue"
                        "redactErrorForReport" "scrubString" "scrubEvent"
                        "scrubLegacyPayloadSecrets" "describeBytes" "hashPin" "pbkdf2"))`,
    ],
  },

  checkFlow(ctx, path) {
    const s = ctx.loc(path.source)
    const where = s ? `${s.file}:${s.line}:${s.column}` : 'unknown'
    ctx.report(path.sink, `secret flows to sink (source ${where}, ${path.steps.length} steps)`)
  },
})
