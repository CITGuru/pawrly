# Variables

Variables are named inputs for sources. You can define them using the `variables:` block, then use `${var:NAME}` anywhere in source config or table definitions.

Use variables for values that should not be hard-coded into a source: API hosts, workspace ids, API keys, bearer tokens, or OAuth credentials. Compared with raw `${secret:}` / `${env:}` [interpolation](./config.md#secrets), variables give each source an explicit input contract and keep names scoped to the sources that use them.

## Basic Example

Most sources only need a plain value and a secret:

```yaml
variables:
  API_BASE:
    kind: variable
    default: https://api.example.com

  API_TOKEN:
    kind: secret
    input: EXAMPLE_API_TOKEN

sources:
  - name: example
    kind: http
    config:
      base_url: ${var:API_BASE}
      token: ${var:API_TOKEN}
```

`API_BASE` resolves from its default. `API_TOKEN` resolves from the configured secret chain using the `EXAMPLE_API_TOKEN` key. Pawrly substitutes those values only at the reference sites.

Declaring a variable does not inject it into a source. A variable matters only when the source references it with `${var:NAME}`.

## Declaration

Each variable has a `kind`:

- **variable** is non-secret configuration, such as a base URL, tenant id, or region. It may have a `default` and a `type` (see [Value Types](#value-types)), and its value can appear in introspection output.
- **secret** is sensitive configuration, such as an API key, token, password, or OAuth credential. It is masked, may not have a `default`, and is always an opaque string (no `type`).

Use `description` to explain what the user should provide during setup:

```yaml
variables:
  DATADOG_SITE:
    kind: variable
    default: datadoghq.com
    description: Datadog site, for example datadoghq.eu

  DATADOG_API_KEY:
    kind: secret
    input: DATADOG_API_KEY
    description: Datadog API key with read access
```

Names may not start with `__pawrly`. A name that looks like a credential (`token`, `secret`, `password`, `api_key`, `bearer`, ...) must be declared as `kind: secret`.

## Required and Optional

A variable is **required** by default: if a source references it and no value can be found (no env / secret value and no `default`), loading fails with an error telling you what to set. Set `required: false` to make the reference **optional** — an unresolved `${var:NAME}` then resolves to `null` instead of failing the load.

```yaml
variables:
  PROXY_URL:
    kind: variable
    required: false   # unset ⇒ null, not an error
```

This works for both kinds — an optional `kind: secret` (for example, send no auth header when no token is provided) resolves to `null` when unset rather than erroring. A `default` always satisfies a reference, so `required` has no effect when a `default` is present. Use an optional reference only where the source config tolerates a `null` (typically an optional field).

## Value Types

A `kind: variable` carries a `type` describing the shape of its value. It defaults to `string`, so existing declarations need no change. A resolved value is inlined with its real JSON type — a `number` becomes `100`, a `boolean` `true` — not a quoted string, so typed source config receives real scalars.


| `type`    | Inlined as          | Notes                                              |
| --------- | ------------------- | -------------------------------------------------- |
| `string`  | text                | the default when `type` is omitted                 |
| `integer` | JSON integer        | whole numbers only                                 |
| `number`  | JSON number         | integer or floating point                          |
| `boolean` | JSON boolean        | env values `true/false`, `1/0`, `yes/no`, `on/off` |
| `enum`    | the matching choice | requires a `choices` list                          |


```yaml
variables:
  PAGE_SIZE:
    kind: variable
    type: number
    default: 100

  VERIFY_SSL:
    kind: variable
    type: boolean
    default: true

  REGION:
    kind: variable
    type: enum
    choices: [us, eu, ap]
    default: us
    description: Datacenter region

sources:
  - name: api
    kind: http
    config:
      region: ${var:REGION}
      page_size: ${var:PAGE_SIZE}
      verify_ssl: ${var:VERIFY_SSL}
```

Rules:

- `type` is only valid on `kind: variable`; secrets are always opaque strings.
- `choices` is required for `type: enum` and rejected for any other type.
- A `default` must match the declared `type`, and for `enum` it must be one of the `choices`.
- A value supplied through an environment variable (always text) is parsed into the declared type; a value that does not parse — `"maybe"` for a `boolean`, or a string outside an `enum`'s `choices` — is a load error.

A `${var:NAME}` reference is substituted only when the whole config value is the placeholder (`page_size: ${var:PAGE_SIZE}`). Typed inlining (a real number or boolean) applies only in that whole-value form.

### Caveats

- **Use the right type for the field.** If a config field expects a string, keep the variable as `type: string` or omit `type`. Use `number`, `integer`, and `boolean` only where the source expects those real values.
- **Use variables inside `config:` and table definitions.** Avoid putting `${var:}` in top-level source fields such as `raw_table: ${var:RAW}`. Management commands read those fields before variables are resolved, so a placeholder string can fail where a boolean or number is expected.

## Static Values

Static values are available as soon as Pawrly loads the config. Use them for hosts, tenant ids, API keys, and pasted bearer tokens.

Pawrly looks for a static value in this order:

1. A value **set with Pawrly** using `pawrly variables set` or `pawrly source connect`. This always wins over the values below.
2. `input`. For `kind: variable`, this reads an environment variable. For `kind: secret`, this reads the configured secret chain (`env` -> `keyring` -> `file`), the same way `${secret:NAME}` does. If omitted, `input` defaults to the variable name.
3. `default` - allowed only for `kind: variable`.
4. If nothing resolves: a load error for a required variable (the default), or `null` for an optional one (`required: false`). See [Required and Optional](#required-and-optional).

For non-secret variables, a stored value is a per-machine override. It lets you change something like `REGION` or `PAGE_SIZE` without editing config or environment variables. Pawrly checks the stored value against the variable's `[type` and `choices](#value-types)` when you set it.

To set a value, run either of:

```sh
pawrly source connect example          # set up everything the source needs
pawrly variables set DATADOG_API_KEY   # set one variable by name (secret or not)
```

`pawrly source connect <source>` checks the variables used by that source. It prompts, without echo, for missing static secrets and starts OAuth for unconnected OAuth variables. By default, it only asks for values that still need setup. Add variable names to update specific values, or use `--all` to review everything.

`pawrly source add` runs the same setup after adding a source. In a terminal, it prompts for static secrets and offers to connect OAuth variables. In a non-interactive context, it prints the `pawrly source connect` command to run instead. Non-secret `kind: variable` values are not prompted; provide them through an environment variable or a `default`.

## OAuth Values

Use OAuth when Pawrly should get the credential for you instead of reading a pasted value. OAuth is valid only on `kind: secret`.

```yaml
variables:
  GH_TOKEN:
    kind: secret
    oauth:
      grant: { type: device_code }
      endpoints:
        device_authorization_url: https://github.com/login/device/code
        token_url: https://github.com/login/oauth/access_token
      client:
        id: { default: my-public-client-id }
      scopes:
        scope:
          delimiter: space
          values: [repo, read:org]
```

Interactive OAuth setup happens before queries. Connect the source that references the OAuth variable:

```sh
pawrly source connect <source>
```

The refresh token is stored by Pawrly and is never written into config. During queries, Pawrly uses that stored token to refresh access. If the variable has not been connected yet, the query asks you to run `pawrly source connect <source>`.

`pawrly source add` also detects unconnected OAuth variables for a new source and offers to connect them.

### OAuth Grants

Pawrly supports three grants:

- `client_credentials` is for machine-to-machine APIs. Pawrly gets a token without asking a user to approve anything.
- `device_code` is for user approval when Pawrly cannot receive a browser callback. Pawrly prints a URL and code, and the user approves in a browser.
- `authorization_code` is for providers that require a browser redirect. Pawrly opens the authorization flow, listens for the local redirect, exchanges the code, and stores the refresh token. Use `pkce: required` when the provider requires PKCE.

Pawrly ships no built-in OAuth client ids. Put a public client id in the spec with `default`, or read a per-user client id with `input`.

### OAuth Examples

`client_credentials`:

```yaml
variables:
  SF_TOKEN:
    kind: secret
    oauth:
      grant: { type: client_credentials }
      endpoints:
        token_url: https://login.example.com/oauth2/token
      client:
        id: { default: my-client-id }
        secret: { input: SF_CLIENT_SECRET }
      scopes:
        scope:
          delimiter: space
          values: [api]
```

`device_code`:

```yaml
variables:
  GH_TOKEN:
    kind: secret
    oauth:
      grant: { type: device_code }
      endpoints:
        device_authorization_url: https://github.com/login/device/code
        token_url: https://github.com/login/oauth/access_token
      client:
        id: { default: my-public-client-id }
      scopes:
        scope:
          delimiter: space
          values: [repo, read:org]
```

`authorization_code`:

```yaml
variables:
  OKTA_TOKEN:
    kind: secret
    oauth:
      grant: { type: authorization_code, pkce: required }
      redirect:
        uri: http://127.0.0.1/callback
      endpoints:
        authorization_url: https://example.okta.com/oauth2/v1/authorize
        token_url: https://example.okta.com/oauth2/v1/token
      client:
        id: { input: OKTA_CLIENT_ID }
        secret: { input: OKTA_CLIENT_SECRET }
      scopes:
        scope:
          delimiter: space
          values: [openid, offline_access]
```

### Redirects

`authorization_code` grants require a `redirect` block so Pawrly knows where to receive the browser callback:

```yaml
redirect:
  uri: http://127.0.0.1:5000/callback
```

For local setup, use a loopback URL such as `http://127.0.0.1/callback`. If the URI has no port, Pawrly listens on any free port and sends that full callback URL to the provider during setup.

Only pin the port when the provider requires an exact callback URL:

```yaml
redirect:
  uri: http://127.0.0.1/callback
  port: 5000
  # port_mode: random|fixed optional
```

You can also put the port directly in `redirect.uri`, such as `http://127.0.0.1:5000/callback`. Do not set the port in both places. `redirect.port_mode` defaults to `random` when no port is set and `fixed` when a port is set; set it explicitly only when you need to be precise.

### Endpoint Discovery

Providers that publish an OpenID Connect discovery document let you skip the per-grant URLs. Set `endpoints.discovery` to the provider's `.well-known/openid-configuration` URL and Pawrly reads `authorization_endpoint`, `device_authorization_endpoint`, and `token_endpoint` from it the first time the variable is used.

```yaml
variables:
  OKTA_TOKEN:
    kind: secret
    oauth:
      grant: { type: authorization_code, pkce: required }
      redirect:
        uri: http://127.0.0.1/callback
      endpoints:
        discovery: https://example.okta.com/.well-known/openid-configuration
      client:
        id: { input: OKTA_CLIENT_ID }
        secret: { input: OKTA_CLIENT_SECRET }
      scopes:
        scope:
          delimiter: space
          values: [openid, offline_access]
```

The discovery URL must be `https` (or `http` to a loopback host for local development). The document is fetched lazily and cached on disk for 24 hours. Any explicit endpoint you also set under `endpoints` overrides the discovered value, so you can use discovery for most URLs and pin one by hand.

## Multiple Auth Methods

A secret can offer more than one way to get a value. For example, prefer OAuth but allow the user to paste an existing token:

```yaml
variables:
  GH_TOKEN:
    kind: secret
    methods:
      - type: oauth
        label: Connect with GitHub
        grant: { type: device_code }
        endpoints:
          device_authorization_url: https://github.com/login/device/code
          token_url: https://github.com/login/oauth/access_token
        client:
          id: { default: my-public-client-id }
      - type: input
        label: Paste token
        input: GITHUB_TOKEN
```

When a variable offers more than one method, `pawrly source connect <source>` shows a menu using each method's `label`:

```text
`GH_TOKEN` offers more than one method:
  1) Connect with GitHub
  2) Paste token
  3) Skip
Choose [1-3]:
```

Choosing the OAuth method starts the connect flow. Choosing the input method prompts for a value without echoing it.

A pasted value is stored by Pawrly and overrides OAuth at load time, so it takes effect immediately. To switch back to OAuth, run `pawrly source connect <source> GH_TOKEN` again and choose the OAuth method.

`pawrly variables set GH_TOKEN` is the non-interactive version of the paste path. It works for any secret that offers an `input` method, including a multi-method secret like the one above. An OAuth-only secret cannot be set this way; connect it with `pawrly source connect`.

Use the shorthand forms for one method:

- `input: ENV_KEY` for one static input.
- `oauth: { ... }` for one OAuth method.

Use `methods:` only when setup should offer a choice. A variable may use either a shorthand or `methods:`, not both.

## Scopes

`variables:` can be declared in four places:

1. **Global** - a top-level `variables:` block, visible to every source.
2. **Fragment file** - a `variables:` block in an included file, visible to that file's sources and nested includes.
3. **Single-source file** - a top-level `variables:` block in a bare one-source file.
4. **Source-local** - a `variables:` key inside one source, visible only to that source.

A source sees the variables along its include chain. Inner declarations shadow outer ones.

Variables use their own namespace, separate from `${secret:}` and `${env:}`. The same name in two unrelated sources refers to two independent variables. Two sources share a value only when both reference the same declaration, such as a global variable or one from a shared include.

## Commands

```sh
pawrly variables list              # show declared variables
pawrly variables set <name>        # set one static variable by name (secret or non-secret)
pawrly source connect <source>     # set up everything a source needs (secrets + OAuth)
```

## Storage

Pawrly stores values it manages itself, such as values set with `pawrly variables set`, secrets collected by `pawrly source connect`, non-secret overrides, and OAuth refresh tokens.

Values are stored per variable declaration, not just by name. That means two sources can both have a variable called `TOKEN` without colliding. This store is also separate from `${secret:NAME}`, so raw secrets and declared variables stay in different namespaces.

Where a value lives depends on how Pawrly obtains it:

- **Provided manually** (`pawrly variables set`) or **set up for a source** (`pawrly source connect`): Pawrly uses the OS keyring when it can. If no keyring is available, it uses encrypted files under `<home>/variables/`. The encryption key comes from `$PAWRLY_TOKEN_KEY` when set; otherwise Pawrly creates one and saves it to `<home>/variables/key` with owner-only permissions.
- **Running on a server, CI, Docker, or another headless setup**: Set `PAWRLY_NO_KEYRING=1` for both `pawrly source connect` and the engine. This makes both processes use the encrypted file store, which avoids session-only keyrings that one process can write but another cannot read.
- **Stored by Pawrly**: These values are never written to config and are never stored as plaintext on disk. A stored value wins over the same variable's `input`, environment value, `default`, or secret-chain value. For `kind: variable`, this gives you a per-machine override for non-secret config.
- **Inherited** from the environment or secret store via `input`, or from a `default`: Pawrly does not copy these into its own store. It reads them from the original source each time the config loads.

## Inspecting Variables

Query `system.variables` to see what variables are declared and whether Pawrly can resolve them now:

```sql
SELECT source, key, kind, type, required, available
FROM system.variables;
```


| Column          | Meaning                                                |
| --------------- | ------------------------------------------------------ |
| `source`        | `global` or the source that owns the variable          |
| `key`           | Variable name                                          |
| `kind`          | `variable` or `secret`                                 |
| `type`          | Value type. Secrets are always `string`.               |
| `value`         | Non-secret value. `NULL` for secrets and OAuth values. |
| `default_value` | Declared `default`, if any                             |
| `description`   | Declared `description`, if any                         |
| `required`      | Whether the value must be provided                     |
| `available`     | Whether Pawrly can provide the value now               |


Find variables that still need setup with:

```sql
SELECT key
FROM system.variables
WHERE NOT available;
```

