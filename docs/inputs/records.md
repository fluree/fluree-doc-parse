# Records: declared XML and JSON

```bash
fdoc convert article.xml --source-format news-article.json -f doco
fdoc convert story.json --source-format formats.json -f md
```

A content system exports what it holds as a record: a news article as the
ten fields its publisher stores it under, written in XML or in JSON. Read as
a file, a record is markup with some prose in it. An extractor finds the
markup as readily as the prose, the offsets count escaped characters nobody
sees, and the publication date is one more string.

So a record is read in two steps that are kept apart:

1. **Its notation**, XML or JSON, into fields: every value the record
   holds, where the record keeps it, as a person reads it.
2. **Its source format**, a declaration of what each field is, into a
   document: content becomes elements, facts become statements about the
   document, and values from a controlled list become the concepts they
   name.

Nothing is inferred. `body` is prose to read, `published` is a date about
the document and `section` is a value from a list the organisation keeps,
and nothing in the notation tells them apart. The declaration does. It is
written once for a kind of record, by someone who knows it, and applied to
every record of that kind.

Like a transcript, a record is a flow: `page` is `0` throughout, `bbox` is
absent, and there is nothing to escalate.

## An example

A record, as a newsroom's system exports it:

```xml
<?xml version="1.0" encoding="utf-8"?>
<record>
  <id>48213</id>
  <body>The city announced the closure on Thursday.
Traffic will be routed through the tunnel.</body>
  <title>Harbour bridge to close for repairs</title>
  <lead>The bridge closes on Monday for six weeks.</lead>
  <published>20260914063000</published>
  <section>transport</section>
  <keywords>Bridges; roads; ferries</keywords>
  <editor>null</editor>
  <desk>Metro</desk>
</record>
```

Its declaration, `news-article.json`:

```json
{
  "id": "https://example.org/format/news-article",
  "label": "News article",
  "match": { "syntax": "xml", "root": "record", "required": ["id", "title", "body"] },
  "documentClass": "https://example.org/model#NewsArticle",
  "nullValues": ["null"],
  "title": "title",
  "created": "published",
  "content": [
    { "field": "title", "as": "title" },
    { "field": "lead" },
    { "field": "body" }
  ],
  "metadata": [
    { "field": "id", "property": "https://example.org/model#recordId" },
    { "field": "published", "property": "https://example.org/model#published",
      "datatype": "http://www.w3.org/2001/XMLSchema#dateTime" }
  ],
  "enums": [
    { "field": "section", "property": "https://example.org/model#section",
      "schemes": ["https://example.org/id/sections"],
      "values": { "Transport": "https://example.org/id/section/transport" } },
    { "field": "keywords", "property": "https://example.org/model#keyword",
      "separator": ";",
      "values": { "bridges": "https://example.org/id/keyword/bridges",
                  "roads": "https://example.org/id/keyword/roads" } }
  ]
}
```

The text, from `-f text`:

```
Harbour bridge to close for repairs

The bridge closes on Monday for six weeks.

The city announced the closure on Thursday.

Traffic will be routed through the tunnel.
```

The record stores its body before its title. The document opens with its
title all the same, because content is read in the declaration's order and
not the record's.

The document node, from `-f doco`:

```json
{
  "@id": "urn:fluree-doc-parse:article/element/0",
  "@type": [ "doco:Document", "https://example.org/model#NewsArticle" ],
  "dcterms:created": { "@type": "xsd:dateTime", "@value": "2026-09-14T06:30:00" },
  "dcterms:title": "Harbour bridge to close for repairs",
  "doc:sourceFields": { "@type": "@json", "@value": [ … ] },
  "https://example.org/model#keyword": [
    { "@id": "https://example.org/id/keyword/bridges" },
    { "@id": "https://example.org/id/keyword/roads" },
    "ferries"
  ],
  "https://example.org/model#published": {
    "@type": "http://www.w3.org/2001/XMLSchema#dateTime",
    "@value": "2026-09-14T06:30:00"
  },
  "https://example.org/model#recordId": "48213",
  "https://example.org/model#section": { "@id": "https://example.org/id/section/transport" }
}
```

`ferries` is not in the list the declaration gives for `keywords`. It is
stated as it was written, and `fdoc` says so on stderr:

```
note: article: `/record/keywords` holds `ferries`, which the list of `keywords` does not: stated as written
```

## Fields

The first step reads the notation and gives no field a meaning. Each field
has three parts:

| part | what it is | XML | JSON |
|---|---|---|---|
| `path` | the value's address in the record. One value, one path. | `/record/tags/tag[2]` | `/tags/1`, a JSON Pointer |
| `name` | what the record calls the field, the same for every value of it. This is the name a declaration uses. | `tags/tag` | `tags` |
| `value` | the value as a person reads it: character references decoded, the notation's escapes undone | `Q&A`, not `Q&amp;A` | `"live"`, not `\"live\"` |

In XML:

- Every element holding text is a field, and so is every attribute:
  `item/@id`. Namespace declarations (`xmlns`, `xmlns:…`) are not fields.
- The name leaves the root out. Every field of a record is under its root,
  so a declaration that names `title` means the record's.
- A path carries an index only where a name repeats among siblings. It is
  1-based, as XPath counts. A reader addresses `/record/title`, and
  `/record/title[1]` would put a number that says nothing in every path of
  every record.
- CDATA sections are read as the text they hold.

In JSON:

- Every string, number and boolean is a field. `null` is the notation's own
  way of saying there is no value, so it is no field.
- An index is a position and not a name: every item of `tags` is a value of
  the field `tags`, at `/tags/0`, `/tags/1`.
- Fields keep the order the record wrote them in.

In both, a value is trimmed, and a field whose value is empty is not a
field.

## The declaration

A declaration is a JSON object. `--source-format` takes a file holding one
declaration, or a list of them.

| key | type | meaning |
|---|---|---|
| `match` | object, required | how a record of this format is told from any other; see below |
| `id` | string | what the declaration is called. Messages name it. |
| `label` | string | a name for people. Messages use it when there is no `id`. |
| `documentClass` | absolute IRI | the class a document of this format is. It stands beside `doco:Document`, not in its place. |
| `nullValues` | list of strings | values that mean there is none; see [Null values](#null-values) |
| `title` | field name | the field holding the document's title, stated as `dcterms:title` |
| `created` | field name | the field holding when the document was created, stated as `dcterms:created` |
| `modified` | field name | the field holding when it was last modified, stated as `dcterms:modified` |
| `content` | list | the fields that are text to read, in the order the document reads them |
| `metadata` | list | the fields that are facts about the document |
| `enums` | list | the fields whose value is one of a controlled list |
| `tracks` | list | the tracks of a recording to read; see [Media assets](media-assets.md#declaring-a-format) |

`match`:

| key | type | meaning |
|---|---|---|
| `syntax` | `xml`, `json` or `axf`, required | the notation |
| `root` | string | the record's root: the root element of XML, the class of a media asset. Any root when absent. Not allowed for JSON, which names no root. |
| `required` | list of field names | fields a record of this format always holds, each with a value that is not a [null value](#null-values) |

An entry of `content`:

| key | type | meaning |
|---|---|---|
| `field` | field name, required | the field to read |
| `as` | `title`, `heading` or `text` | `title` is a `doco:SectionTitle` of level 1, `heading` one of level 2, `text` is paragraphs. `text` when absent. |

An entry of `metadata`:

| key | type | meaning |
|---|---|---|
| `field` | field name, required | the field holding the fact |
| `property` | absolute IRI, required | the property the value is stated under |
| `datatype` | absolute IRI | the value's datatype. A plain string when absent. See [Typed values](#typed-values). |

An entry of `enums`:

| key | type | meaning |
|---|---|---|
| `field` | field name, required | the field holding the value |
| `property` | absolute IRI, required | the property the value is stated under |
| `values` | object | the list: each value as the record writes it, and the IRI of the concept it names |
| `separator` | string | what separates the values of a field that holds several |
| `schemes` | list of absolute IRIs | the concept schemes the values belong to, the one to look in first coming first. Informative. |
| `classes` | list of absolute IRIs | the classes a value's concept may be. Informative. |
| `matchOn` | `label`, `notation` or `id` | whether the record names a concept by its label, by its code (`skos:notation`) or by its identifier, the last step of its IRI. `label` when absent. Informative. |

An absolute IRI has the scheme `http`, `https` or `urn` and no white space.
A prefixed name such as `model:recordId` is refused.

A declared field name covers the fields under it. A declaration that names
`body` means what `body` holds: its value, or the values of `body/p` where
the record nests them.

A field plays one part: it is named once, in one of `content`, `metadata`
and `enums`. Declared twice, one declaration would win without a word and
the other would state nothing, so the declaration is refused. `title`,
`created` and `modified` are apart from this: in the example, `published`
is both the document's `created` and a fact stated under the model's own
property.

A field inside a declared one is that one's. `body/p` is part of `body`,
so a declaration that names both is refused as well, whichever list each is
in and whichever comes first. Two names that only begin alike, `body` and
`bodyguard`, are two fields.

A declaration is checked when it is read and not when a record meets it. A
mistake in a declaration is the same mistake for every record, and the
place to hear of it is where the declaration was written. These are
refused:

- A declaration that would match everything. A JSON format must name
  `required` fields, and an XML format must name a `root` or `required`
  fields.
- A property, class, datatype, scheme or concept that is not an absolute
  IRI.
- A field declared twice, in one of `content`, `metadata` and `enums` or
  in two of them.
- A declared field that lies inside another declared field.
- A key the declaration does not have. A misspelt `nulValues` is an error,
  not a declaration silently without null values.

The message names the file, the declaration and the field:

```
error: article.xml: --source-format v1.json: https://example.org/format/news-article: `id` is declared as metadata twice
error: article.xml: --source-format v2.json: https://example.org/format/news-article: `title` is declared as content and as metadata; a field plays one part
error: article.xml: --source-format v3.json: https://example.org/format/news-article: `body` is declared as content and `body/p` as metadata, and one holds the other; a field plays one part
```

## Recognition

A record is of a format when its notation is the format's `syntax`, its root
is the format's `root` where one is given, and it holds every `required`
field.

A required field is held when it holds a value that is not one of the
format's null values. An export that writes `null` into a field it has
nothing for has not filled the field in. A required field is named the way
the other declarations name fields, so a container is held when a field
inside it is: `"required": ["body"]` recognises
`<body><p>One.</p></body>`.

A root named `record` or `item` is shared by formats that have nothing else
in common, and JSON names no root at all. The fields a record must hold are
what makes the match a recognition and not a coincidence.

Matching a record against the declared formats has three outcomes:

| outcome | when | `fdoc convert` |
|---|---|---|
| `Known` | exactly one format recognises the record | reads it as that format declares |
| `Unknown` | no format does | fails for a file named as a record: `no declared source format recognises this record`. See [Detection](#detection). |
| `Ambiguous` | more than one does | fails, and names the formats that claim it |

The names are those of `Recognition`, which
`fluree_doc_record::recognise` returns.

Two formats claiming one record is an error and never a guess, because the
guess would decide what every field means. An unknown record is an error
too and not a fallback, where the file is named as a record: the
declarations were passed to say what the inputs are.

## Null values

An export writes `null` or `N/A` into a field it has nothing for.
`nullValues` lists those values, and a field holding one is read as absent:
it becomes no element, it states nothing, and it is left out of
`doc:sourceFields`. In the example, `editor` holds `null` and appears
nowhere in the output.

The comparison ignores surrounding space and nothing else: `null` does not
cover `NULL`. The empty value is always absent, and needs no entry.

Null values are the format's and not the notation's. Until a format says
otherwise, `null` in an XML element is a value like any other. Once it
does, a field holding one is not held when `required` is checked either.

## Content and paragraphs

Content is read in the declaration's order. Where a record holds several
values of one field, they are read in the record's order.

A `title` or a `heading` is one line: line breaks and runs of white space
inside it become single spaces.

A `text` field becomes one `doco:Paragraph` per paragraph of its value:

- Where the value separates paragraphs with blank lines, a line break
  inside a paragraph is a wrap, and the lines are joined with a space.
- Where the value has no blank line, every line is a paragraph. That is how
  an export writes prose it holds in one field.

In the example, `body` holds two lines and no blank line, so it reads as two
paragraphs. Both carry the path `/record/body`.

## Typed values

A `metadata` value with no `datatype` is stated as a plain string, as
written. With a `datatype`, it is stated as a typed literal, and for the
types a store compares by value it is checked and written in the type's own
form:

| datatype (`xsd:`) | accepted | stated as |
|---|---|---|
| `dateTime` | `20250424153338`, `2025-04-24 15:33:38`, `2025-04-24T15:33:38` | `2025-04-24T15:33:38` |
| `date` | `20250424`, `2025-04-24`, or any of the forms above | `2025-04-24` |
| `time` | `183000`, `18:30:00` | `18:30:00` |
| `integer`, `int`, `long`, `short` | a whole number | `042` as `42` |
| `decimal`, `double`, `float` | a number | as written |
| `boolean` | `true`, `false`, `1`, `0` | `true` or `false` |

What follows the seconds of a time written with colons, a fraction or a
zone, is kept as written: `2025-04-12T06:26:14.821Z` stays as it is.

These are checked because a date that is not a date sorts as a string among
dates and is found by no range. Any other datatype is the declaration's to
know: the value is stated under it as written.

A value that does not fit its type is stated as a plain string, as written,
and reported:

```
note: odd: `/record/published` holds `14/09/2026`, which is not a `http://www.w3.org/2001/XMLSchema#dateTime`: stated as written
```

The fields named by `created` and `modified` are read the same way, as a
date or a date and time. `dcterms:created` is an `xsd:dateTime`, or an
`xsd:date` where the record states a day and no time. A value that is not a
date leaves the property unstated, and is reported.

The record is read in every case. What did not fit is on stderr, and in
`Converted::warnings` for a Rust caller.

## Enums

An enum field holds a value from a list the organisation keeps: a section, a
region, a channel. `values` maps each value as the record writes it to the
concept it names, and the value is stated as a reference to that concept:

```json
"https://example.org/model#section": { "@id": "https://example.org/id/section/transport" }
```

A value is matched without regard to case, accents or surrounding space.
Those are how a value was typed and not which value it is: one archive
holds `Zürich`, `Zurich` and `ZURICH`, and means one city. In the
example, the record's `transport` and `Bridges` are found under the list's
`Transport` and `bridges`.

With a `separator`, a field holds several values, and each is stated on its
own. A property stated more than once has its values in the order the
record gives them, each once: two fields that name the same concept, a city
by its name and by its code, state it once.

A value the list does not hold is stated as a plain string, as written, and
reported. The record is still read.

This reader holds no list. `schemes`, `classes` and `matchOn` say where the
values come from, which kind of concept is meant and what the keys of
`values` are. They are for whoever resolves the list into `values` before a
record is read, and change nothing in how a record is read.

## What every output carries

Every element read from a record says which field it was read from:

| output | the element's field | what the record states about the document |
|---|---|---|
| [JSON](../formats/json.md#records-carry-their-field) | `sourcePath` | not in this output |
| [DoCO](../formats/doco.md#records-say-which-field) | `doc:sourcePath` | on the document node: the declared class, the declared properties, `dcterms:title`, `dcterms:created`, `dcterms:modified` and `doc:sourceFields` |
| [XHTML](../formats/xhtml.md), [Markdown](../formats/markdown.md), [text](../formats/text.md) | not carried | not carried |

The address is a path and not an offset into the file. The file's own
characters are escaped and encoded, so a count against them is a count
against something nobody reads. The path names the field, and the element's
text is that field's value, or one paragraph of it, as a person sees it.

`doc:sourceFields` is the record as the source held it: its fields in the
record's order, each with the part it plays, including the fields the
declaration says nothing about. The declared properties are statements of
the graph, and a consumer asks the graph for them. This is what lets a
reader shown the document be shown the record it came from.

```json
[
  { "path": "/record/id", "property": "https://example.org/model#recordId",
    "role": "metadata", "value": "48213" },
  { "path": "/record/body", "role": "content" },
  { "path": "/record/title", "role": "content" },
  { "path": "/record/lead", "role": "content" },
  { "path": "/record/published", "property": "https://example.org/model#published",
    "role": "metadata", "value": "2026-09-14T06:30:00" },
  { "iri": "https://example.org/id/section/transport", "path": "/record/section",
    "property": "https://example.org/model#section", "role": "enum", "value": "transport" },
  …
  { "path": "/record/keywords", "property": "https://example.org/model#keyword",
    "role": "enum", "value": "ferries" },
  { "path": "/record/desk", "role": "unmapped", "value": "Metro" }
]
```

| key | meaning |
|---|---|
| `path` | the field's address, the same form as an element's |
| `role` | `content`, `metadata`, `enum` or `unmapped` |
| `value` | the value, as it is stated. Absent for content, whose value is the text of the elements that carry the path. A value of a checked type, and a date named by `created` or `modified`, is in the type's own form. A value that does not fit is as the record writes it. |
| `truncated` | `true` when the value was longer than 1000 characters and is cut there. Past that length a field is a payload and not a fact. |
| `property` | the property the value is stated under, for `metadata` and `enum`, as an absolute IRI |
| `iri` | the concept an enum value names. Absent when the list does not hold the value. |

An `unmapped` field is one the declaration gives no part: it is kept here
and stated nowhere in the graph. A field holding several enum values has
one entry for each. A field read as absent has none.

A field named by `title`, `created` or `modified` and given no other part
is listed as `metadata`, under the Dublin Core term the document node
states it by:

```json
{ "path": "/record/title", "property": "http://purl.org/dc/terms/title",
  "role": "metadata", "value": "Harbour bridge to close for repairs" }
{ "path": "/record/published", "property": "http://purl.org/dc/terms/created",
  "role": "metadata", "value": "2026-09-14T06:30:00" }
```

The value is shown as the document node states it: the title on one line,
a date as the ISO 8601 date it was read as. The record writes
`20260914063000`. A value that is not a date is shown as written, and the
document's date is left unstated.

A field that is also content, metadata or an enum is listed as that, in
that order. In the example, `title` is content and `published` is stated
under the model's own property, so neither is listed under a Dublin Core
term.

Elements carry the provenance `xml` or `json`, and the evidence `declared`:
the class of every element is what the format declares its field to be. See
[Provenance and evidence](../concepts/provenance.md).

## Detection

XML and JSON are read as records only when `--source-format` is given.
Without it, `fdoc` has no reader for them, and the command fails.

With it, a record is recognised by its **content**: the file opens with `<`
or `{`, after an optional byte-order mark and white space, and one of the
declarations recognises it. So a record exported as `.txt`, or with no
extension, is still read as one. UTF-8 and UTF-16 are both read. A
byte-order mark says which, and with none a file whose second byte is zero
is read as UTF-16, since no record opens with a NUL character.

The name decides what happens to a file that no declaration recognises, or
that is not well-formed:

| the file | recognised by one declaration | recognised by none, or not well-formed |
|---|---|---|
| named `.xml` or `.json`, or read from stdin | read as the declaration says | an error |
| named anything else | read as the declaration says | left to the other readers |

A file named `.xml` or `.json` that opens with neither `<` nor `{` is an
error too. Most often it is a JSON list of records:

```
error: batch.json: not a record: a record is one XML element or one JSON object, and this file opens with neither
```

An HTML page opens with `<` too. It is not named as a record, so when no
declaration recognises it, it is read as the HTML page it is, and records
and other documents convert in one run. A file named `.xml` was passed as
a record, so there a declaration that does not fit is a mistake to hear
of. A file that more than one declaration recognises is an error whatever
its name.

With `--source-format` given, a directory is also scanned for `.xml` and
`.json` files. Without it, it is not. Keep the declarations outside the
directories you convert: a declaration is a `.json` file itself, is picked
up with the records, and fails as a record that no declaration recognises.

## In Rust

```rust
use fluree_doc_record::{convert, read, recognise, Recognition, SourceFormat};

let formats = vec![SourceFormat::from_json(&declaration)?];
let record = read(&bytes)?;
match recognise(&record, &formats) {
    Recognition::Known(format) => {
        let doc = convert(&record, format);
        // doc.elements, doc.notes for the emitters, doc.warnings
    }
    Recognition::Ambiguous(claimed) => { /* more than one format claims it */ }
    Recognition::Unknown => { /* no declared format is this record's */ }
}
```

## Limitations

- **One record per file.** A JSON file that is a list of records is a
  batch, and which of its records a document is about is not something a
  reader can decide. `fluree_doc_record::json::read` refuses it, and so
  does `fdoc` for a file named `.json`. Read from stdin, a list is not
  taken for a record and fails as the PDF it is not: `parse: Invalid`. An
  XML file that wraps several records in one root is read as one record
  whose fields repeat.
- **Mixed content** is read as the element's own text followed by its
  children. `<body>Before <b>bold</b> after</body>` is a field `body`
  holding `Before` and `after` on two lines, then a field `body/b` holding
  `bold`. An element is a field or a container of fields here; markup
  inside a value is a document's business.
- **Markup escaped inside a value is not parsed.** A value holding
  `&lt;p&gt;One.&lt;/p&gt;` is read as the text `<p>One.</p>`, tags
  included.
- **A JSON number is written as the parser prints it**: `1.50` is `1.5`,
  and `1e3` is `1000`.
- **Content is titles, headings and paragraphs.** A record yields no list
  and no table.

## Errors

A file that is not well-formed in its notation is
`RecordError::Malformed`. One that is well-formed and is not a record, such
as an XML document with no element or a JSON value that is not an object,
is `RecordError::NotARecord`. A declaration that cannot be read, or that
validation refuses, is an error for every record it was passed for: no
record is read by a declaration that is wrong.
