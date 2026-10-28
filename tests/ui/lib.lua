-- Tiny assertion helpers for serpis UI tests (run via scripts/ui-test.sh).
local T = { results = {} }

function T.check(name, got, want)
  local ok = got == want
  table.insert(T.results, (ok and "PASS " or "FAIL ") .. name .. (ok and "" or (" got=[" .. tostring(got) .. "] want=[" .. tostring(want) .. "]")))
end

-- Run keys in a fresh text buffer and compare its text.
function T.text_case(name, text, keys, want)
  local id = serpis.buffer.create("text", "*uitest*", { text = text })
  serpis.buffer.show(id)
  serpis.keys.feed(keys)
  T.check(name, serpis.buffer.text(), want)
  serpis.buffer.kill(id)
end

-- Type a key sequence the way a keyboard delivers it: plain characters (and SPC) as text,
-- everything else as key events with modifiers. Unlike serpis.keys.feed, this goes through
-- the platform input path, so it catches bindings real keys can't reach.
local NAMED = { SPC = "Space", RET = "Enter", ESC = "Escape", TAB = "Tab", DEL = "Backspace",
  ["<up>"] = "ArrowUp", ["<down>"] = "ArrowDown", ["<left>"] = "ArrowLeft", ["<right>"] = "ArrowRight",
  ["<pageup>"] = "PageUp", ["<pagedown>"] = "PageDown" }
function T.press(seq)
  local d = serpis.debug
  for tok in seq:gmatch("%S+") do
    local mods, rest = {}, tok
    while #rest > 2 and rest:sub(2, 2) == "-" and rest:sub(1, 1):match("[CMsS]") do
      local m = ({ C = "ctrl", M = "alt", s = "cmd", S = "shift" })[rest:sub(1, 1)]
      table.insert(mods, m)
      rest = rest:sub(3)
    end
    local plain = #mods == 0
    if plain and (rest == "SPC" or #rest == 1) then
      local c = rest == "SPC" and " " or rest
      if rest == "SPC" then d.key("Space") end
      d.text(c)
    elseif NAMED[rest] then
      d.key(NAMED[rest], mods)
    else
      if rest:match("^%u$") then table.insert(mods, "shift") end
      d.key(rest:upper(), mods)
    end
  end
end

function T.finish()
  local f = io.open(os.getenv("SERPIS_TEST_OUT"), "w")
  f:write(table.concat(T.results, "\n"), "\n")
  f:close()
  serpis.debug.quit()
end

return T
