# Media assets: AXF

```bash
fdoc convert episode.axf -f doco
fdoc convert episode.axf --source-format episode-format.json -f md
```

AXF, the Asset eXchange Format, is what Avid's asset management (Interplay
MAM, MediaCentral Asset Management) exports an asset as: a programme, an
episode, a news story, with everything the archive knows about it. It is XML
by notation and a small database by content. This reader turns it back into
what the recording is called and what was said in it:

```
Evening news

Good evening. The harbour bridge closes on Monday for six weeks.

How long is the detour?

About ten minutes.

In sport, the final is on Sunday.
```

An asset is read the way a [record](records.md) is, in two steps: the
container into fields and tracks, then a source format that says what each
of them is. The container is Avid's and is the same everywhere, so it needs
no declaration. What is in it is the archive's: the classes, the field
names and the tracks are the data model each site configures, and a track
named `SITE_RUNDOWN` means something to one archive only. Which field is prose and which track
is speech is therefore a declaration's to say. With none, the reader reads
what every asset has.

Like a transcript, an asset is a flow: `page` is `0` throughout, `bbox` is
absent, and there is nothing to escalate.

## The container

```xml
<?xml version="1.0" encoding="utf-16"?>
<AXFRoot>
  <MAObject type="default" mdclass="EPISODE">
    <GUID dmname="">a-100</GUID>
    <Meta name="MAINTITLE" format="string">Evening news</Meta>
    <Meta name="REGISTRATION_DATETIME" format="string">20260914183000</Meta>
    <Meta name="MODIFICATION_DATETIME" format="string">20260915090210</Meta>
    <Meta name="BROADCAST_DATE" format="string">20260914</Meta>
    <Meta name="CHANNEL" format="string">2</Meta>
    <StratumEx name="CLOSED_CAPTION">
      <Group orderidx="0" id="0">
        <Segment id="0" contentid="c-1" begin="1000" end="3500" />
        <Segment id="1" contentid="c-2" begin="5000" end="7500" />
        …
      </Group>
    </StratumEx>
    <StratumEx name="STORIES">
      <Group orderidx="0" id="0">
        <Segment id="0" contentid="s-2" begin="20000" end="40000" />
        <Segment id="1" contentid="s-1" begin="5000" end="20000" />
      </Group>
    </StratumEx>
  </MAObject>
  <MAObject type="default" mdclass="S_CLOSED_CAPTION">
    <GUID dmname="">c-1</GUID>
    <Meta name="CLOSED_CAPTION" format="string">Good evening.</Meta>
  </MAObject>
  …
  <MAObject type="default" mdclass="S_STORIES">
    <GUID dmname="">s-1</GUID>
    <Meta name="NUMBER" format="string">1</Meta>
    <Meta name="TITLE" format="string">Harbour bridge closes</Meta>
  </MAObject>
  …
  <MVAttribute type="CONTRIBUTORS" index="0" mdclass="EPISODE" objectid="a-100">
    <Meta name="NAME" format="string">Anna Reyes</Meta>
    <Meta name="FUNCTION" format="string">12</Meta>
  </MVAttribute>
  <MVAttribute type="CONTRIBUTORS" index="1" mdclass="EPISODE" objectid="a-100">
    <Meta name="NAME" format="string">Tom Okafor</Meta>
    <Meta name="FUNCTION" format="string">7</Meta>
  </MVAttribute>
</AXFRoot>
```

| in the file | what it is |
|---|---|
| `AXFRoot` | the root every export has |
| `MAObject` | an object, with its class in `mdclass` |
| `GUID` | the object's identifier |
| `Meta` | a field of the object: a name and a value, always a string |
| `StratumEx` | a track of the asset: one timeline of the recording, such as its captions or its stories |
| `Segment` | a stretch of a track, from `begin` to `end` in milliseconds. `contentid` is the `GUID` of the object that holds what the segment says. |
| `MVAttribute` | one value of a multi-valued attribute: a group of `Meta` fields, written beside the objects. `type` is the attribute's name, `index` numbers its values, and `objectid` is the `GUID` of the object the value is about. |

So a caption is three things in three places: a segment on the caption
track, the object the segment points at, and that object's one field. The
reader puts them back together: each segment of a track is given the fields
of the object it points at, beside its own timing.

**The asset** is the object the others are about: the one no segment points
at. An export lists it first, and that is not relied on.

**A multi-valued attribute** is what an asset has several of: its
contributors, its other titles, the places it was shot in. Each value is an
element of its own and holds fields the way an object does. The reader gives
them to the object `objectid` names, after that object's own fields and in
the file's order. An attribute naming no object is the asset's, or, written
inside an object, that object's. One naming an object the export left out
is nobody's and is not read. A field holding nothing is not a field, as on
an object.

## Paths

Every field and every track has a path:

| path | what it addresses |
|---|---|
| `GUID` | the asset's identifier |
| `Meta:MAINTITLE` | a field of the asset |
| `Stratum:CLOSED_CAPTION` | a track |
| `Stratum:CLOSED_CAPTION/Meta:CLOSED_CAPTION` | a field of a track's segments |
| `Attribute:CONTRIBUTORS[1]/Meta:NAME` | a field of one value of a multi-valued attribute, numbered as the file numbers it |

An element read from a field carries the field's path, and one read from a
track carries the track's. The path of a segment's field is in the `Record`
a Rust caller reads, and in no output.

A declaration names a field by its name alone, `MAINTITLE`, and a track by
its name, `CLOSED_CAPTION`. A field of a multi-valued attribute is named by
the attribute and the field, `CONTRIBUTORS/NAME`, and has as many values as
the attribute has: an attribute's field is often called what a field of the
asset is, a title among them, and is not a value of that one. The asset's
class is the record's root, so a
declaration can be written for one class:
`"match": { "syntax": "axf", "root": "EPISODE" }`.

## Undeclared

With no declaration for it, an asset is read by what every asset has,
whatever the archive's data model. These are Avid's own fields, which a
site does not rename:

| in the asset | in the output |
|---|---|
| `MAINTITLE` | the document's title: a `doco:SectionTitle` of level 1, and `dcterms:title` |
| `REGISTRATION_DATETIME` | `dcterms:created`. Listed in `doc:sourceFields` as `metadata`, under that term, with the date in ISO 8601. |
| `MODIFICATION_DATETIME` | `dcterms:modified`, listed the same way |
| the tracks that are plainly captions | one `doco:Paragraph` per turn |
| every other field | kept in `doc:sourceFields` as `unmapped`, and stated nowhere |

A track is plainly captions when every segment of it that holds anything
holds exactly one field of its own, the same one throughout, whatever a
multi-valued attribute says of it. That is what a caption or a transcript
line is. A track of stories holds a number and a title, a
track of locators a colour and a user, and neither is taken for speech.
Where several tracks qualify, the one with the most captions is read, and
the others are not: captions and a transcript of the same recording are the
same words twice.

The dates are written as the asset manager stores them, `20260914183000`,
and stated in ISO 8601:

```json
{
  "@id": "urn:fluree-doc-parse:episode/element/0",
  "@type": "doco:Document",
  "dcterms:created": { "@type": "xsd:dateTime", "@value": "2026-09-14T18:30:00" },
  "dcterms:modified": { "@type": "xsd:dateTime", "@value": "2026-09-15T09:02:10" },
  "dcterms:title": "Evening news",
  "doc:sourceFields": { "@type": "@json", "@value": [ … ] }
}
```

## Declaring a format

A declaration for the archive's data model says what its own fields and
tracks are. It is a [source format](records.md#the-declaration) whose
`syntax` is `axf`, with one more key, `tracks`:

```json
{
  "id": "https://example.org/format/episode",
  "match": { "syntax": "axf", "root": "EPISODE" },
  "documentClass": "https://example.org/model#Episode",
  "title": "MAINTITLE",
  "created": "REGISTRATION_DATETIME",
  "modified": "MODIFICATION_DATETIME",
  "content": [ { "field": "MAINTITLE", "as": "title" } ],
  "metadata": [
    { "field": "BROADCAST_DATE", "property": "https://example.org/model#broadcastDate",
      "datatype": "http://www.w3.org/2001/XMLSchema#date" }
  ],
  "enums": [
    { "field": "CHANNEL", "property": "https://example.org/model#channel",
      "matchOn": "notation",
      "values": { "2": "https://example.org/id/channel/television" } }
  ],
  "tracks": [
    { "track": "CLOSED_CAPTION", "field": "CLOSED_CAPTION", "role": "speech" },
    { "track": "TRANSCRIPT", "field": "TRANSCRIPT", "role": "speech" },
    { "track": "STORIES", "field": "TITLE", "role": "sections" }
  ]
}
```

An entry of `tracks`:

| key | type | meaning |
|---|---|---|
| `track` | track name, required | the track to read |
| `field` | field name, required | the field of its segments that holds the text |
| `role` | `speech` or `sections`, required | what the track holds |

| role | what the track holds | becomes |
|---|---|---|
| `speech` | what was said: captions or a transcript | one `doco:Paragraph` per speaker turn |
| `sections` | the parts of the recording, each with a title: a bulletin's stories | one `doco:SectionTitle` per part, carrying the part's start and end |

Of the tracks declared for a role, the first that the asset holds text in
is read, and the others are not. The example declares the captions before
the transcript, so an asset that has both is read from its captions, and
one that has only a transcript is read from that.

Unlike XML and JSON, an `axf` format may name neither a `root` nor
`required` fields: any asset is an asset. It then claims every asset.

The same asset, read as declared, in Markdown:

```markdown
# Evening news

(00:01) Good evening.


## Harbour bridge closes

(00:05) The harbour bridge closes on Monday for six weeks.

(00:09) How long is the detour?

(00:12) About ten minutes.


## Cup final

(00:20) In sport, the final is on Sunday.
```

And what its document node states, beside the title and the dates:

```json
"@type": [ "doco:Document", "https://example.org/model#Episode" ],
"https://example.org/model#broadcastDate": {
  "@type": "http://www.w3.org/2001/XMLSchema#date",
  "@value": "2026-09-14"
},
"https://example.org/model#channel": { "@id": "https://example.org/id/channel/television" }
```

## Sections and turns

Captions are grouped into speaker turns by the rules a caption file's cues
are, with the same thresholds: see
[Transcripts](transcripts.md#turns). Sections add three rules:

- **Tracks are sorted by start time.** A track is kept in the order its
  segments were logged, which is not always the order they play in. In the
  example, the second story is logged before the first.
- **What is said belongs to the section that was open when it began.** A
  caption that starts before the first section comes before it.
- **A turn never runs across the start of a section.** The first words of a
  story are not the last of the one before, however short the pause between
  them. In the example, `Good evening.` ends 1.5 seconds before the next
  caption starts, which is within a turn's pause. Undeclared, the two are
  one turn. Declared, a story starts between them, and they are two.

A section's title is a `doco:SectionTitle` of level 2 when the document has
a title, and of level 1 when it has none. A segment of the sections track
that holds no title is left out.

## A change of voice

Broadcast captions name no speaker. What they mark is the change of one: a
line opening with a dash is another voice, the convention of subtitling
since before there were files to put it in.

```
- How long is the detour?
- About ten minutes.
```

The dash is that mark and not a word, so it opens a turn and is left out of
the text. A hyphen, an en dash and an em dash are read alike. A dash before
a digit is a negative number and a dash before another dash is a rule, so
neither is a mark: `-5 degrees this morning` keeps its dash and opens
nothing.

This applies to captions handed over by a container. In a `.vtt` or `.srt`
file a cue's text is left as written: what a dash means there is the
file's.

A dash says that the voice changes and not whose it is. So the turn it
opens has no speaker, even where the captions label their speakers and the
turn before it has one. These four captions:

```
Ada Park: Good evening.
Ada Park: The harbour bridge closes
on Monday for six weeks.
- How long is the detour?
```

are two turns:

```json
{ "text": "Ada Park: Good evening. The harbour bridge closes on Monday for six weeks.",
  "turn": { "speaker": "Ada Park", "start_ms": 1000, "end_ms": 9800 } }
{ "text": "How long is the detour?",
  "turn": { "start_ms": 9800, "end_ms": 12000 } }
```

An unlabelled caption with no dash continues whoever was speaking, as it
does in a [transcript](transcripts.md#speakers).

## What every output carries

| output | the track or field | times |
|---|---|---|
| [JSON](../formats/json.md#records-carry-their-field) | `sourcePath` | `turn.start_ms`, `turn.end_ms`, on turns and on section titles |
| [DoCO](../formats/doco.md#records-say-which-field) | `doc:sourcePath` | `doc:startMs`, `doc:endMs`, on turns and on section titles |
| [XHTML](../formats/xhtml.md) | not carried | `data-start-ms`, `data-end-ms`, on turns |
| [Markdown](../formats/markdown.md) | not carried | `(00:05)`, the turn's start |

A turn carries the path of the track it was read from,
`Stratum:CLOSED_CAPTION`, and a section title the path of its own,
`Stratum:STORIES`. Times are milliseconds from the start of the recording,
and stay out of the text for the reason they do in a
[transcript](transcripts.md#what-a-turn-carries).

```json
{
  "id": "elem-00003",
  "type": "doco:SectionTitle",
  "page": 0,
  "text": "Harbour bridge closes",
  "level": 2,
  "turn": { "start_ms": 5000, "end_ms": 20000 },
  "sourcePath": "Stratum:STORIES",
  "provenance": "axf",
  "evidence": "declared"
}
```

In `doc:sourceFields`, the asset's fields come first, in the asset's order,
then each track that was read, as one entry of role `content`. A track that
was not read has no entry. See
[Records](records.md#what-every-output-carries) for the keys of an entry.

## Detection

An asset is recognised by its **content**: its root element is `AXFRoot`.
The exports are named `.axf`, which no system has a type for, and are
UTF-16, which makes them binary to anything that looks at bytes. UTF-8 is
read too.

`fdoc convert episode.axf` needs no option, and a directory is scanned for
`.axf` files. A file named `.axf` whose root is not `AXFRoot` is not read
as an asset. The same content check applies to `fdoc convert -` on stdin.

With `--source-format`, an asset that one declaration recognises is read as
declared. One that none recognises is read undeclared, where an XML or JSON
record would be an error. One that several recognise is an error.

## Limitations

- **One asset per export.** Where an export holds several objects that no
  segment points at, the first is the asset and the others are not read.
- **A segment pointing at an object the export left out** keeps its place
  in time and says nothing. A track made of such segments holds no text, so
  it is read neither as speech nor as sections.
- **Times are taken as milliseconds.** A segment whose `begin` or `end` is
  not a whole number is left out. One that ends before it begins is given
  its beginning as its end.
- **The fields of a segment cannot be declared as metadata.** A track is
  read for one field, its speech or its titles. A story's number stays in
  the file.

## Errors

A file whose root is not `AXFRoot` is not an asset: `AxfError::NotAxf`. One
that is not well-formed XML is `AxfError::Malformed`. An export that holds
no object is `AxfError::NoAsset`. An asset in which the format finds no
content is not an error: it converts to nothing, and `fdoc` prints a note to
stderr saying which format it was read as.

In Rust, the reader hands over a record, and the record reader makes the
document:

```rust
let record = fluree_doc_axf::read(&bytes)?;
let format = fluree_doc_axf::default_format(&record);
let doc = fluree_doc_record::convert(&record, &format);
// doc.elements, doc.notes for the emitters, doc.warnings
```
