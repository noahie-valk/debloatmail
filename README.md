# debloatmail

This project is in pre-alpha. Core libraries, parsing engines, and storage primitives are actively being built. It's not even close to ready yet, daddy, chill out.

Also this is vibe-coded, so if you don't like that, you're a retard.

---

Email client/RSS reader with **more security** and **less bloat**.

Thunderbird is bloated because it bundles a full browser rendering engine (Gecko) and attempts to be an all-in-one suite with calendars, address books, chat, and HTML webmail formatting. `debloatmail` strips all of that away in favor of a lean, secure, plaintext-first client that treats incoming email and RSS feeds as items in a single, unified stream.

To further reduce bloat on one's system, `debloatmail` uses a thin local cache instead of a full disk replica of the mailbox. This could be a good option if you already self-host your email (you're retarded), but it's built for those who use mainstream providers in mind first. Why? Because they do all the other fucking work, of course!

Read the docs for more technical specifications, but it's written in shitty AI prose, so don't say I didn't warn ya!
