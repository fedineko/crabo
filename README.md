# What?

`Crabo` is `Fedineko` component that produces snapshot for a given URL.

Snapshot consists of:

* title
* description
* image

It is used in `Fedineko` to present preview images for URLs referenced in
indexed content. There is no sophisticated logic, `Crabo` just
captures information returned by content serving API or provided
in meta-tags of HTML page such as Open Graph properties.

# Why does Crabo access my site?

**TL;DR**: your site was mentioned in ActivityPub document published
in Fediverse.

`Crabo` is service component under `Fedineko` umbrella.

`Octofedi` component of `Fedineko` listens on ActivityPub relays
for any public notes, forwards these to `Oceanhorse` for processing
and - if allowed by content author - indexing.

One of these processing steps before actual indexing is _enrichment_,
it includes producing preview images for URLs mentioned in note.

Depending on URL `Crabo` either calls video
hosting service API or parses HTML documents pointed to by URL.

In the latter case you will see `fedineko/crabo-x.x` or
`Fedineko (crabo/x.x.x; +https://fedineko.org/about)` user-agent in web-server
logs. The former one is older variant of user-agent string, the latter is
new format that all `Fedineko` components will use eventually.

# How do I stop Crabo from accessing my web-site?

## robots.txt

`Crabo` identifies itself as `fedineko-crabo` and follows `robots.txt` guidance.

Adding something like this to deny access to pages under `/path`

```text
User-agent: fedineko-crabo
Disallow: /path/*
```

or this (applicable to all robots and any documents)

```text
User-agent: *
Disallow: /
```

instructs `Crabo` to not access disallowed locations. There will be no attempt
to access URL if it matches disallow rule.

**NB**: Currently `robots.txt` is cached for one day so **updates might
not change `Crabo` behaviour for about 24 hours**.

## robots meta-tags

`Crabo` follows `robots` meta-tags instructions:

```html
<meta name="robots" content="noindex">
<meta name="fedineko-crabo" content="nosnippet">
<meta name="fedineko-crabo, some-other-bot" content="none">
```

by basic substring match.

Even if `robots.txt` permits access, `robots` meta-tags could deny it.

Collecting meta-tags requires parsing HTML page, so it will be downloaded.

## Firewalling

Fedineko uses less than five shared VM instances, all in the same data center,
so setting firewall rule to block IP address/subnet will do the magic.

Keep in mind that IP addresses of VM instances are likely to change over time
as cluster could scale up or nodes be retired/replaced.

As this blockage is unknown to `Crabo`, it will still attempt to fetch page.
However, if there are too many connection errors within short period of time,
site is suppressed for fifteen minutes or so, any Fedineko requests to produce
snapshots of pages hosted on the site are ignored.

# License

Apache 2.0 or MIT.