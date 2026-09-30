# Transcripts: WebVTT and SubRip

```bash
fdoc convert call.vtt -f doco
fdoc convert episode.srt -f md
```

Zoom, Teams and most notetakers export a meeting's transcript as WebVTT;
video subtitles come as WebVTT or SubRip. Both cut speech into cues of a few
seconds, each under a timing line. This reader turns them back into what was
said: one `doco:Paragraph` per speaker turn.

```
Ada Park: Thanks, everyone. Before we start, I want to say this is my last review.

Ben Ortiz: Morning.
```

Like DOCX, a transcript is a flow: `page` is `0` throughout, `bbox` is
absent, and there is nothing to escalate.

## What a turn carries

The speaker stays **in the text**, as `"<speaker>: "` before the words. The
text projection is what an extractor reads and what every character offset
counts against, and a claim with its speaker attached is a different fact
from the bare words. The label always occupies the first *n* characters of
the text, where *n* is the length of the speaker's name in characters, so a
consumer can link that span to a person without parsing the text again.

The speaker and the times are also fields of their own:

| output | speaker | times |
|---|---|---|
| [JSON](../formats/json.md) | `turn.speaker` | `turn.start_ms`, `turn.end_ms` |
| [DoCO](../formats/doco.md#transcripts-say-who-spoke-and-when) | `doc:speaker` | `doc:startMs`, `doc:endMs` |
| [XHTML](../formats/xhtml.md) | `data-speaker`, and `<b>` around the label | `data-start-ms`, `data-end-ms` |
| [Markdown](../formats/markdown.md) | `**Ada Park**` | `(04:32)`, the turn's start |

Times are milliseconds from the start of the recording: the start of a
turn's first cue and the end of its last. They stay **out of the text** in
every output except Markdown, because an extractor that meets `04:32` in the
prose reads it as a time the speaker mentioned. Markdown is written for
people, who want to know where to scrub to.

## Speakers

A cue's speaker comes from, in order:

1. **A voice tag**, `<v Ada Park>` or `<v.loud Ada Park>`, as Teams writes
   them. The name is what follows the classes.
2. **A label at the start of the cue text**, `Ada Park: `, as Zoom and most
   notetakers write them. A label is accepted when the same label starts at
   least two cues, when it is a numbered label (`Speaker 2`, `SPEAKER_01`),
   or when it is two to four capitalised words. So `Okay: so, where were we`
   is words, not a speaker called Okay. Display names that carry pronouns or
   a company, such as `Ada Park (she/her)`, are labels too, and are reported
   whole.
3. Otherwise, no one.

Some tools label only the cue where the speaker changes. So in a file that
labels speakers in the text, a cue with no label continues the current
speaker's turn when it follows within two seconds. In a file that uses voice
tags, an untagged cue has no speaker.

Speakers are reported as the file names them. Working out that `Speaker 2`
is a particular person is left to you.

## Turns

The file states when each cue starts and ends, and usually who is speaking.
It says nothing about turns, because a caption tool cuts every few seconds
and one sentence runs across several cues. So the reader groups cues into
turns:

- Consecutive cues from one speaker are one turn, and a new speaker starts a
  new one.
- A pause of more than **2 seconds** ends a turn, even when the same speaker
  goes on. This is what breaks captions with no speakers, such as lecture
  and video subtitles, into paragraphs.
- A turn that has run **60 seconds** ends at the next cue that closes a
  sentence, so that a claim in a long answer still has a start time near it.
  One that has run **120 seconds** ends at the next cue whether or not a
  sentence has closed, since machine captions are often unpunctuated.
- Rolling captions, where each cue repeats the line above it before adding
  a new one, are read once.
- A cue that tags two voices becomes two turns.

A dash at the start of a cue is left as written in a `.vtt` or `.srt` file.
It is read as a change of voice only for captions handed over by a
container; see [Media assets](media-assets.md#a-change-of-voice).

The same rules and thresholds apply to both formats. As Rust constants they
are `PAUSE_MS`, `LONG_TURN_MS` and `MAX_TURN_MS` in `fluree-doc-transcript`.

## What is kept

| in the file | in the output |
|---|---|
| the `WEBVTT` header and header lines | dropped |
| `STYLE` and `REGION` blocks | dropped |
| a `NOTE` before the first cue | its own paragraph, lines kept, with no `turn`. Notetakers put the meeting's title and date here. |
| a `NOTE` among the cues | dropped. These annotate cues (confidence scores, editing remarks), are not speech, and do not break a turn. |
| cue identifiers and timing lines | the turn's `start_ms` and `end_ms` |
| cue settings (`line:90% align:start`), SubRip coordinates | dropped |
| `<i>`, `<b>`, `<u>`, `<c.class>`, `<lang>`, `<ruby>`, inline timestamps, SubRip's `<font>` and `{\an8}` | markup removed, words kept |
| `&amp;`, `&lt;`, `&gt;`, `&nbsp;`, numeric references | decoded |
| `&lrm;`, `&rlm;` | removed |

A cue's lines are joined with spaces.

Zoom and Teams exports carry no date. It is in the file name or the meeting
platform, so pass it alongside the transcript if you need it.

## Detection

A transcript is recognised by its **content** first, so a `.vtt` saved as
`.txt` or uploaded without a type still reads as a transcript:

- **WebVTT**: the file opens with `WEBVTT` (after an optional byte-order
  mark), followed by a space, a tab, a line break or nothing.
- **SubRip**: the first cue is a number alone on a line, followed by a timing
  line.

Files named `.vtt` or `.srt` are read as transcripts too. The same content
check applies to `fdoc convert -` on stdin.

Timings may omit the hours (`04:32.404`), and SubRip's comma
(`00:04:32,404`) is accepted in either format. Files may use CRLF line
endings. WebVTT is UTF-8. For SubRip, a UTF-8 or UTF-16 byte-order mark is
honoured, and a file that is not UTF-8 is read as Windows-1252.

## Empty and unreadable files

A WebVTT file with no cues converts to nothing, and `fdoc` prints a note to
stderr saying so, so an empty transcript can be told apart from one that was
not read. The library returns an empty `Vec` for it rather than an error.

A file with no `WEBVTT` header and no cues is not a transcript:
`fluree_doc_transcript::parse` returns `TranscriptError::NoCues`, and
`fdoc` exits with an error.
