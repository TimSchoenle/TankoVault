> **This is a sample, not legal text.** It exists so a fresh instance boots with a working
> `[legal]` section and a rendering `/legal/privacy` page. Replace every word of it with a policy
> written for your deployment, by you or by counsel, before anyone else uses this instance.

# Data Policy

## What this instance stores about you

- **Account** — the email address and display name you registered with, and a hash of your
  password. The password itself is never stored.
- **Library** — the series you track, their status, and how far you have read.
- **Sessions** — the devices currently signed in, so you can revoke them.
- **Audit trail** — privileged and privacy-relevant actions. Whether your IP address and browser
  are recorded here is an operator setting; _state what this instance does._
- **Taste profile** — see below.

## Recommendations, and the profile behind them

This instance builds a **taste profile** from your library in order to recommend series: which
tags, authors, formats and lengths you gravitate towards, which ones you appear to avoid, and how
strongly. It is derived from what you track, how far you have read, when you last read it, and
anything you have dismissed — nothing else. It is not shared, not sold, and not used to make any
decision about you other than which series to put on a shelf.

The profile is automated and it is a profile in the sense of the GDPR, so:

- **You can see it.** `Account → Taste profile` shows exactly what the system believes about you,
  in the same terms it uses internally.
- **You can correct it** by changing your library, or by dismissing a recommendation. Both take
  effect on your next visit.
- **You can export it.** It is included in your data export, alongside the per-series scores it
  was computed from.
- **It disappears with your account**, and it can be rebuilt from your library at any time — it
  holds nothing your library does not already say.

Recommendations are computed **from your own library only**. This instance does not compare you
with other readers.

_If this instance has switched recommendations off, none of the above applies and no profile is
built._

## Cookies and local storage

One cookie holds your refresh token, which is what keeps you signed in. Your appearance and
language choices are kept in your browser's local storage and never sent anywhere.

## Third parties

- **Sources** — chapter listings are fetched by the server, not by your browser, so browsing this
  instance does not expose you to the sources it reads. Following a link out does.
- **External trackers** — if you link an account such as AniList, your reading progress is sent to
  that service. Unlink it and this stops.

## Your rights

You can export your library, ask for a copy of everything held about you, and delete your account
from Account → Privacy & data. _Name the contact address for requests here._

## Retention

Deleting your account removes it and your library. Audit records are kept for the operator's
configured retention window; _state it here._
