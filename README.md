# debloatmail

This project is in pre-alpha. Core libraries, parsing engines, and storage primitives are actively being built.

Also this is vibe-coded, and I am most certainly a beginner programmer. However, this is mostly a learning exercise.

---

Email client/RSS reader with **more security** and **less bloat**.

Thunderbird is bloated because it bundles a full browser rendering engine (Gecko) and attempts to be an all-in-one suite with calendars, address books, chat, and HTML webmail formatting. `debloatmail` strips all of that away in favor of a lean, secure, plaintext-first client that treats incoming email and RSS feeds as items in a single, unified stream.

To further reduce bloat on one's system, `debloatmail` uses a thin local cache instead of a full disk replica of the mailbox, and an engine coded with Rust. This could be a good option if you already self-host your email, but it's built for those who use mainstream providers in mind first. Why? Because they do all the other work, of course!

[Email me](mailto:noahie@fastmail.com) if you have any suggestions or advice :)
