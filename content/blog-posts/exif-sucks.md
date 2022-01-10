title = 'Exif sucks'
description = "There might be reasons, but that doesn't make it better"
first_published = 'Sun, 09 Jan 2022 00:16:14 -0800'
updated = []
tags = ['rants', 'exif', 'photography']
is_hidden = true
+++

Exif -- Exchangeable image file format. You might already be acquainted, but for the uninitiated:
it's the most widely-adopted image metadata format. You've probably heard brief mention to it before
-- usually about GPS information being stored in photos. Exif is where that information's stored.
The only issue is that the format and its history make it excessively & unnecessarily difficult to
work with.

This is not a story about failure, but one of phyrric victory -- eventually building the system I
wanted, but at the cost of my sanity. This post is a walk-through of some of the problems and
"quirks" I ran into.

## Background

#### 1: Who is this person ranting in your computer screen? Why do they care about Exif so much?

These are all good questions. To be completely honest, I don't have any prior relationship with
Exif; I'm relatively new to photography and (currently) I'm not particularly invested the future of
Exif, either -- I just wanted to get information about [my photos] from the actual files themselves.

[my photos]: https://sharnoff.io/photos

That being said, I'm the sort of person that finds it hard to sit with badly designed systems -- so
the kinds of problems that I'll mentioning are the sort that become particularly noticeable when you
poke at it a bit.

#### 2: How do people normally deal with Exif?

I don't think people normally do. That being said, there is a _wonderful_ utility for reading and
writing Exif data -- [ExifTool] -- that I cannot recommend enough. Even being able to view the
source was hugely helpful while I was working on this. It's immensely complicated (300k lines of
Perl), and actually handles more than just Exif. We'll talk about it some more later.

[ExifTool]: https://exiftool.org/

## The goal

As part of making the very website you're seeing this on, I wanted to display information for each
photo -- things like the location, shutter speed, and ISO. Oh - and the title. And a description.
And the name of the camera that took the photo, too.

So there's actually a number of things to fetch from the image. At this point I should stress: this
is both entirely possible **and** completely within the bounds of the Exif spec. This isn't some
case of contorting the functionality of a system to serve a purpose other than what it was designed
for. Rather, I don't think the design of Exif was _ever_ good or reasonable.

## Part 1: Diving in

Let's just dive straight into trying to read metadata from an image.

The first thing we'll get from the image is its title. For this, you have to know a little bit about
the library I was using -- a very reasonable Rust crate to read Exif data from an image:
[`kamadak-exif`](https://github.com/kamadak/exif-rs). It doesn't support writing Exif data (at time
of writing), but that's fine; I don't need it to.

So: before we can actually read any of this meta information for an image, we have to actually parse
the Exif data. Thankfully `kamadak-exif` makes this pretty simple:

```rust
// -- imports omitted --

struct ExifData {
    // ... lots of fields
}

fn read_exif(path: &Path) -> ExifData {
    let file = File::open(path).expect("couldn't read image file");
    let mut buf_reader = BufReader::new(&file);

    let exif_data = exif::Reader::new()
        .read_from_container(&mut buf_reader)
        .expect("couldn't read Exif data");

     // ... do something with the exif data
}
```

If you aren't familiar with Rust, don't worry -- we won't be seeing too much more.

### Part 1b: If we already need the image data, can't we give it the raw bytes?

As it turned out, I actually already needed to read the full image files to produce smaller
versions. So ideally, we'd give this library a slice of bytes from the image header instead of
passing a reference to the file on disk. Unfortunately, the only way to read the Exif data with this
library is through an object that implements [`io::BufRead`] and [`io::Seek`] (the latter of which
allows the current read location to be moved arbitrarily). There _are_ implementations of these for
byte slices, but why do we have to use the entire file?

[`io::BufRead`]: https://doc.rust-lang.org/stable/std/io/trait.BufRead.html
[`io::Seek`]: https://doc.rust-lang.org/stable/std/io/trait.Seek.html

And at first, this seems silly -- why isn't there a "grab the header bytes and read Exif from it"
method? And why do we need `Seek`? Is this just bad design in the library?

This was the first of many points at which I should have given up to spare my sanity.

Normally, metadata is provided in the header of a file. Exif rejects these notions of implementation
simplicity and instead allows its data to be placed _anywhere_ in the file, broken up arbitrarily.
So even if you can skip to different parts of the file, reading all the data might still require
_arbitrarily many jumps_.

Per the [Wikipedia article](https://en.wikipedia.org/wiki/Exif#Problems):

> The derivation of Exif from the TIFF file structure using offset pointers in the files means that
> data can be spread anywhere within a file, which means that software is likely to corrupt any
> pointers or corresponding data that it doesn't decode/encode. For this reason most image editors
> damage or remove the Exif metadata to some extent upon saving.

Now, it's not all that bad. All my images are JPEGs, and JPEG happens to only allow Exif data in the
JPEG header. Although it's worth noting:

> Exif metadata are restricted in size to 64 kB in JPEG images because according to the
> specification this information must be contained within a single JPEG APP1 segment

So, there's some limitations -- let's just hope the `MakerNote` added by the camera doesn't use up
too much space, right? [^makernote]

Anyways, after that diversion, it seems there isn't a practical way to avoid reading the entire file
-- especially if there's any possibility I want to use something other than JPEG later.

### Part 1c: Back on track

Ok, enough moaning about inefficiencies and bad formatting. Let's actually get the image title,
alright?

It's at this point that I have a bit of a confession to make. While this _was_ actually the first
tag I tried to pull from the image, it did prove to be more confusing than most of the others. A
simple search for "title" in the Exif tags yields no results. As it turns out, the tag we want is
`ImageDescription` -- the [Exif 2.2 specification] has the following to say:

> A character string giving the title of the image. It may be a comment such as "1988 company
> picnic" or the like. Two-byte character codes cannot be used. When a 2-byte code is necessary, the
> Exif Private tag `UserComment` is to be used.

[Exif 2.2 specification]: http://www.exif.org/Exif2-2.PDF

Ok so it's not called `Title` or `ImageTitle`, but `ImageDescription`. And y'know what -- if we
ignore that the _actual_ description is in `UserComment`, this one's not _so_ bad. It's pretty easy
to find (all the occurences of the word "title" in the Exif 2.2 spec lead back to it), but couldn't
they have picked a better name? [^titlename]

```rust
fn read_exif(path: &Path) -> ExifData {
    // ... read Exif

    let value = &exif_data
        .get_field(Tag::ImageDescription, In::PRIMARY)
        .expect("missing ImageDescription tag")
        .value;

    let title = match value {
        Value::Ascii(vs) if vs.len() == 1 => String::from_utf8(&vs[0]).unwrap(),
        v => panic!("expected single ASCII value for ImageDescription tag, found {:?}", v),
    };

    ExifData { title /* ... other fields to be added later */ }
}
```

In any case, it's not too hard to finish getting the title from here. And now, we have our first
introduction to the value types that Exif provides. Unfortunately, we've already arrived at a bit of
a conundrum:

> If ASCII values are allowed to contain _multiple_ strings, what does that mean? How do we join
> them?

Yes, that variable `vs` inside `Value::Ascii` is actually a list of ASCII strings -- and the
specification provides no guidance on how to interpret that.

The Exif specification doesn't offer any clarification on this, and I'm actually not too sure,
either. Normally I wouldn't like to leave something like this unanswered, but perhaps my Google-fu
just isn't quite strong enough. There's probably a note about this buried deep in the
ExifTool soruce, but -- as I've mentioned before -- it's quite complex. Even the _fact_ that there
can be multiple strings requires a little bit of diving -- the Exif spec would have you think that
there can only be one, but the [TIFF 6.0 specification] (which Exif extends), says:

> Any ASCII field can contain multiple strings, each terminated with a NUL. A single string is
> preferred whenever possible. The Count for multi-string fields is the number of bytes in all the
> strings in that field plus their terminating NUL bytes. Only one NUL is allowed between strings,
> so that the strings following the first string will often begin on an odd byte.

[Tiff 6.0 specification]: https://www.itu.int/itudoc/itu-t/com16/tiff-fx/docs/tiff6.pdf

## Part 2: Values

This section (and all the remaining ones) will be more brief. But they aren't any less important;
there's just less for us say.

The Exif specification defines a set of "value types" for image metadata. In fact, the spec lays
them all out pretty clearly:

| Value | Name | Description |
|-------|------|-------------|
| 1 | BYTE | An 8-bit unsigned integer |
| 2 | ASCII | An 8-bit byte containing one 7-bit ASCII code. The final byte is terminated with NUL. |
| 3 | SHORT | A 16-bit (2-byte) unsigned integer |
| 4 | LONG | A 32-bit (4-byte) unsigned integer |
| 5 | RATIONAL | Two LONGs. The first LONG is the numerator and the second LONG expresses the denominator |
| 7 | UNDEFINED | An 8-bit byte that may take any value depending on the field definition |
| 9 | SLONG | A 32-bit (4 byte) signed integer (2's complement notation) |
| 10 | SRATIONAL | Two SLONGs. The first SLONG is the numerator and the second SLONG is the denominator |

These are all actually borrowed from the TIFF 6.0 specification, which defines a few others (we'll
get to them in a moment).

Off the bat, most of this seems pretty reasonable. Rational values are actually quite useful in the
context of digital cameras -- most measures of exposure time, for example, are in integer fractions
of a second.

Unfortunately, it is missing a few things -- like floating point numbers. The TIFF 6.0 spec actually
has both FLOAT and DOUBLE types, but Exif doesn't include them. We _can_ get by without them, but it
cuts off a number of possibilities (e.g. floating-point GPS coordinates[^gps-floats]).

There's some other quirks, too: Some Exif fields are given the type "UNDEFINED" but aren't
_actually_ entirely undefined: `UserComment` is one such field. For these, the first eight bytes of
the "undefined" value are expected to match one of the following:

| Prefix | Meaning |
|--------|---------|
| `ASCII\0\0\0` | Ascii text |
| `JIS\0\0\0\0\0` | JIS-encoded text |
| `UNICODE\0` | "Unicode Standard" (UTF-16) |
| `\0\0\0\0\0\0\0\0` | "Undefined" |

Even after all of this, there's still no way to include UTF-8 text in a widely-recognized format
(you could, of course, give it the leading 8 null bytes to make it undefined, but no tools would
understand it). The fact that UTF-8 wasn't _initially_ available is forgivable; it was only
[first presented in 1993], and only really started taking over the internet after 2006 or so:

[first presented in 1993]: https://en.wikipedia.org/wiki/UTF-8#History

<a title="Chris55, CC BY-SA 4.0 &lt;https://creativecommons.org/licenses/by-sa/4.0&gt;, via Wikimedia Commons" href="https://commons.wikimedia.org/wiki/File:Utf8webgrowth.svg">
    <img alt="Utf8 growth over time; 5% in 2006 to 65% in 2012" src="/Utf8webgrowth.svg">
</a>

But given everything above, it's an absolute shame that newer versions of Exif don't support UTF-8
at all -- even when such a change could easily be made backwards-compatible.

## Part 3: Where's ISO?

The ISO setting is one of those fundamental pieces of digital cameras. It is used to set the "film
speed" for a particular photo (a measure of the camera's sensitivity to light), and the common name
is due to the ubiquity of the ISO rating systems for film and digitcal cameras.

So what should the name of the Exif tag be? Well, it's important to note that the ISO standard for
determining the film speed of digital cameras -- the one that we now just refer to as "ISO" -- was
only first released in 1998, as ISO 12232. Exif has been around since 1995.

Some sensible names might be `FilmSpeed` or something to do with `Sensitivity` -- or even renaming
the field to `ISO`, now that the ISO rating system is basically the only remaining standard.

Instead, there's a history of different names for this field. The furthest information back I could
find was for Exif 2.2 (released 2002), which called this `ISOSpeedRatings`; Exif 2.3 renamed it to
`PhotographicSensitivity`.

Now, to be completely fair, there's actually a reason for this. The ISO 12232 standard actually gave
camera manufacturers a choice of 5 different exposure rating systems to choose from. Exif 2.2.
doesn't have much information attached:

> Indicates the ISO Speed and ISO Latitude of the camera or input device as specified in ISO 12232. 

... but [Exif 2.3] gives plenty:

> This tag indicates the sensitivity of the camera or input device when the image was shot. More
> specifically, it indicates one of the following values that are parameters defined in ISO 12232:
> standard output sensitivity (SOS), recommended exposure index (REI), or ISO speed. Accordingly, if
> a tag corresponding to a parameter that is designated by a SensitivityType tag is recorded, the
> values of the tag and of this PhotographicSensitivity tag are the same. However, if the value is
> 65535 (the maximum value of SHORT) or higher, the value of this tag shall be 65535. When recording
> this tag, the SensitivityType tag should also be recorded. In addition, while "Count = Any", only
> 1 count should be used when recording this tag.
>
> Note that this tag was referred to as "ISOSpeedRatings" in versions of this standard up to Version
> 2.21.

[Exif 2.3]: https://www.cipa.jp/std/documents/e/DC-008-2012_E.pdf

There's some wonderful snippets here: "actually there's multiple ratings now", "just use the maximum
value if your ISO is too big", and "oh btw this was renamed, but it's still the same tag".

In fact, I'd argue that these are actually completely different tags, even though they have the same
ID (0x8827). It even looks like the Exif 2.3 spec would agree -- it adds a new tag, `ISOSpeed`
(0x8833) that isn't subject to some of the same restrictions: the count is officially limited to 1
and it uses `LONG` instead of `SHORT` (allowing ISO values up to 2<sup>64</sup>).

Of course, the primary issue with Exif 2.3 is that it was released in 2010, revised in 2012, and had
two follow-up versions (2.31 & 2.32) in 2016 and 2019, respectively. So adoption is lacking in older
cameras, which mine happens to be. ExifTool fully supports Exif 2.3, but still refers to 0x8827 for
ISO instead of using it as a fallback.

## Part 4: Time zones

For our final section, let's talk about time zones. Those things that programmers hate, right?

Well if I had to guess, I'd say that the designers of the Exif specs weren't overly fond of them
either --- so much so that the ability to include time zone information was only added in 2016, with
Exif 2.31. There were _no_ mechanisms until then -- not even by differing information in other
fields. Technically, one option was to use `GPSTime`, but that value records the time at which the
GPS location was determined -- it can be off by an arbitrary 

Ok, but why does this matter? Surely we haven't had accurate digital information about time zones
accessible for too long, right?

Well... _the_ time zone database (`tz`/`tzdata`) has been around [since 1986] -- before Exif. And
that's just when time zones were _digitized_; of course time zones existed pre-1986. So it seems a
little strange that they'd just omit that information from the standard -- not even making it an
optional field.

[since 1986]: https://en.wikipedia.org/wiki/Tz_database#History

The reason _I_ found this so frustrating is that, because Exif 2.31 was released so recently, most
of the search results for time zones (at time of writing) essentially say
["tough luck, it doesn't exist"], ["I get by with external records"], or
are recent, but [lamenting about the lack of support in tooling].

["tough luck, it doesn't exist"]: https://photo.stackexchange.com/a/96714
["I get by with external records"]: https://photo.stackexchange.com/a/21742
[lamenting about the lack of support in tooling]: https://photo.stackexchange.com/a/97147

## Conclusion

Look --- for the problem it's trying to solve, Exif isn't actually _that_ bad. Especially
considering how long it's been around for.

But it does have some real problems, many of which shouldn't have occured in the first place. And
some that _could_ have been fixed, but now have attempted solutions that only exacerbate the
problem.

There _are_ other formats -- [XMP] is actually the most popular, and most recent products (both
hardware and software) seem to focus on it instead of Exif. Unfortunately, Exif is still the only
option for many digital cameras that are sufficiently old.

[XMP]: https://en.wikipedia.org/wiki/Extensible_Metadata_Platform

My recommendation? Don't use Exif unless you really have to -- XMP supposedly has great support, and
I'd hope that you wouldn't need it. If you do find yourself _really_ needing to use Exif, then just
use ExifTool -- it's 300 thousand lines of code that really does handle almost everything you need.
It's what I'd do now, if I didn't already have working code.

[^makernote]: `MakerNote` contains some manufacturer-specific metadata about the image, without any
  standard format (and sometimes encrypted). There's a number of issues with `MakerNote` that I
  won't get into here, as I didn't personally encounter them -- I'd recommend reading the linked
  Wikipedia article if you're curious.
  For reference, the camera I'm currently using produces `MakerNote`s that are around 20kb in
  length.
  <!--
  TODO: There should be an extra newline, but the markdown parser puts that paragraph outside
  of the div.
  -->

[^titlename]: Of course, this wouldn't be quite as frustrating if they had a strict "no name
  changes" policy. Unfortunately though, that's not the case -- as you'll see in the discussion of
  the ISO tag.

[^gps-floats]: Currently, GPS latitude and longitude are given as three rationals each -- in the
  "degrees, minutes, seconds" format. So any coordinates provided in decimal must first be converted
  from something like `40.7335135, -74.0030787` to `40° 44' 0.6498", -74° 0' 11.0838"`. To be fair,
  this conversion is not _particularly_ complicated, but it does introduce the possibility of
  inaccuracies that need not be there -- especially when most modern programs just use decimal
  degrees.
