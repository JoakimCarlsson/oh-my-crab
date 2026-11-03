# Adding a provider

Every provider is built into the `omc` binary. There are no plugins and no
runtime registration. See [supported.md](supported.md) for what exists today.

A provider is two things kept apart:

- a **wire**, the protocol it speaks, with one streaming implementation per wire
  in `crates/omc-providers/src/wires/`;
- a **definition**, one file in `crates/omc-providers/src/providers/` that names
  the wire, how to sign in, and where the API lives.

Most new providers reuse a wire that already exists, so they are one small file
(case 1). A provider that speaks a protocol crab does not have also needs a wire
(case 2), and one that signs in in a way crab does not support needs a new sign-in
method (case 3).

## 1. It speaks a wire that already exists

This is the common case. Add one file and one line.

1. Create `crates/omc-providers/src/providers/groq.rs`:

   ```rust
   //! Groq, signed in with an API key from the environment.

   use omc_auth::Auth;

   use crate::provider::{Def, Wire};

   /// The definition of the Groq provider.
   pub(super) static DEF: Def = Def {
       id: "groq",
       label: "Groq (API key)",
       wire: Wire::ChatCompletions,
       default_model: None,
       auth: Auth::ApiKey { env: &["GROQ_API_KEY"] },
       base_url: "https://api.groq.com/openai/v1",
       base_url_env: None,
       headers: &[],
   };
   ```

2. In `crates/omc-providers/src/providers.rs`, add `mod groq;` and an entry
   `Groq => groq::DEF,` to the `providers!` list. The macro declares the
   `Provider` variant and `Provider::ALL`; the order of the list is the login
   picker's order.

The `id` is the name saved to disk and equals the variant name in lowercase, so
pick it once and keep it; a test checks this. Then run `cargo test --workspace`
and add a row to [supported.md](supported.md).

The fields of a definition:

| Field           | Meaning                                                                       |
| --------------- | ----------------------------------------------------------------------------- |
| `label`         | What the login picker shows.                                                  |
| `wire`          | Which `Wire` the provider speaks.                                             |
| `default_model` | `Some(id)` for a fixed default, `None` to take the first model the server lists. |
| `auth`          | `Auth::OAuth(&SPEC)`, `Auth::DeviceCode(&DEVICE)`, `Auth::ApiKey { env: &[...] }` or `Auth::None`. |
| `base_url`      | The API address, used by wires that serve many hosts.                         |
| `base_url_env`  | Environment variable that overrides `base_url`, or `None`.                    |
| `headers`       | Headers sent on every request, such as the client identity a host requires.   |

An API-key provider reads its key from the first of its `env` variables that is
set, and signing in fails with the variable names when none is. A provider with
`Auth::None`, such as a local server, needs no key. `claude.rs`, `codex.rs` and
`copilot.rs` in the same folder show an OAuth spec and a device-code spec.

## 2. It needs a new wire

Do step 1 as well, then:

1. Add `crates/omc-providers/src/wires/<wire>.rs`, declare it in `wires.rs`, with a `complete` and a
   `list_models`; take `base_url` too when the wire serves many hosts. Translate
   the neutral types in `crates/omc-providers/src/chat.rs` (`Request`,
   `Message`, `Reply`, `ToolSpec`) to and from the wire's format, and push each
   streamed chunk to the `Sink`. `chat_completions.rs` is the shortest example.
2. Add a `Wire` variant in `crates/omc-providers/src/provider.rs`.
3. Add its arm to the matches the compiler points at: `RemoteModel::complete`
   in `chat.rs`, `list_models` in `lib.rs`, and `context_window` and `efforts`
   in `models.rs`.
4. Test the request translation and the assembly of a streamed response.

## 3. It needs a sign-in that `Auth` does not have

`Auth` covers the browser OAuth flow, the device-code flow (a short code the
user enters on a web page, as Copilot uses), a key from the environment, and no
credentials. For anything else add an `Auth` variant in `crates/omc-auth/src/method.rs`
and handle it in `login` and `credentials_for` in
`crates/omc-auth/src/session.rs`. Credentials are
stored as `Credentials`; keep the secret in `access` and set `expires_at_ms` so
a refresh happens when it should. If the provider serves each account from its
own address, put it in `Credentials::base_url`; the wire uses that instead of
the provider's `base_url`.

A device-code provider is declared with a `Device` in its own file, as
`providers/copilot.rs` does: client id, scope, the code and token endpoints, the headers to
send, and optionally an endpoint that says which API address serves the account.
The UI is told the page and the code through `Event::LoginCode`.

## Trying a provider

Unit tests cover the request translation and the streamed-response assembly and
need no network. To try one by hand, export its key if it has one, start `omc`
and sign in to the provider by its id.
