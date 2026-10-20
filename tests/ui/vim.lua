local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")
local c = T.text_case
c("dw", "foo bar baz", "d w", "bar baz")
c("2dw", "foo bar baz", "2 d w", "baz")
c("ciw", "foo bar baz", "w c i w Q U X ESC", "foo QUX baz")
c("dot repeat", "a b c d", "d w .", "c d")
c("dd", "one\ntwo\nthree", "j d d", "one\nthree")
c("yyp", "one\ntwo", "y y p", "one\none\ntwo")
c("undo", "one two", "d w u", "one two")
c("Vjd", "1\n2\n3\n4", "j V j d", "1\n4")
c("3x", "abcdef", "3 x", "def")
c("A", "abc", "A d e f ESC", "abcdef")
c("o", "a\nc", "o b ESC", "a\nb\nc")
c('ci"', 'say "hello" now', 'f h c i " b y e ESC', 'say "bye" now')
c("dt)", "f(a, b)", "f a d t )", "f()")
c("J", "a\n  b", "J", "a b")
c(">>", "x", "> >", "    x")
c("cw", "foo bar", "c w b a z ESC", "baz bar")
c("D", "keep this", "w D", "keep ")
c("xp", "ab", "x p", "ba")
c("di(", "call(x, y)", "f x d i (", "call()")
c("gg dG", "a\nb\nc", "G g g d G", "")
-- text-yanked hook: ranges are 1-based {line, col}, end exclusive.
local last
serpis.hook.add("text-yanked", function(e) last = e end)
local function yank_case(name, text, keys, want)
  last = nil
  local id = serpis.buffer.create("text", "*uitest*", { text = text })
  serpis.buffer.show(id)
  serpis.keys.feed(keys)
  local got = last and (table.concat(last.start, ":") .. "-" .. table.concat(last["end"], ":") .. (last.linewise and " line" or "")) or "none"
  T.check(name, got, want)
  serpis.buffer.kill(id)
end
yank_case("yank hook yiw", "alpha beta", "w y i w", "1:7-1:11")
yank_case("yank hook yy", "one\ntwo", "y y", "1:1-2:1 line")
yank_case("yank hook visual", "abcdef", "v l l y", "1:1-1:4")

-- C-d / C-u scroll half a page; C-u must beat the global universal-argument binding.
local lines = {}
for i = 1, 200 do lines[i] = "line " .. i end
local id = serpis.buffer.create("text", "*uitest*", { text = table.concat(lines, "\n") })
serpis.buffer.show(id)
-- Paging needs one drawn frame to know the page size.
serpis.defer(300, function()
  serpis.keys.feed("G")
  local bottom = serpis.buffer.cursor()
  serpis.keys.feed("C-u")
  local up = serpis.buffer.cursor()
  T.check("C-u scrolls up", up < bottom, true)
  serpis.keys.feed("C-d")
  T.check("C-d scrolls back down", serpis.buffer.cursor(), bottom)
  serpis.keys.feed("v C-u")
  T.check("C-u in visual", serpis.buffer.cursor(), up)
  serpis.keys.feed("ESC")
  serpis.buffer.kill(id)
  T.finish()
end)
