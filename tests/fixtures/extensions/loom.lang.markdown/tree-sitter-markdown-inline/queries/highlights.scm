[
  (code_span)
  (link_title)
] @string

[
  (emphasis_delimiter)
  (code_span_delimiter)
] @punctuation

(emphasis) @constant

(strong_emphasis) @type

[
  (link_destination)
  (uri_autolink)
] @string

[
  (link_label)
  (link_text)
  (image_description)
] @function

[
  (backslash_escape)
  (hard_line_break)
] @escape

(image
  [
    "!"
    "["
    "]"
    "("
    ")"
  ] @punctuation)

(inline_link
  [
    "["
    "]"
    "("
    ")"
  ] @punctuation)

(shortcut_link
  [
    "["
    "]"
  ] @punctuation)
