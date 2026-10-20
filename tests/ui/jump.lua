-- avy/hop jumps and ninja copies with real keys, in every kind of buffer: text, a split, vim
-- (gw), a chat's composer, and a terminal.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("session-directories", {})

local function label_for(pred)
  for _, l in ipairs(serpis.jump.labels()) do
    if pred(l) then return l end
  end
end
local function press_label(l) if l then T.press((l.label:gsub(".", "%0 "))) end end
local function last_message() local m = d.messages(); return m[#m] or "" end
local function steps(list)
  local i = 0
  local function go() i = i + 1; if not list[i] then return T.finish() end; serpis.defer(list[i]() or 350, go) end
  go()
end

local text, other, chat, term
local target
steps({
  function()
    text = serpis.buffer.create("text", "*jump-a*", { text = "alpha beta gamma\ndelta epsilon zeta\nsee https://example.com/x and 4c898c3 here" })
    serpis.buffer.show(text)
    serpis.buffer.set_cursor(1, 1)
  end,
  function() T.press("M-g") end,
  function() T.press("w") end,
  function()
    local ls = serpis.jump.labels()
    T.check("labels on every word", #ls >= 10, true)
    target = label_for(function(l) return l.buffer == text and l.line == 2 and l.col == 7 end)
    T.check("epsilon is labelled", target ~= nil, true)
    press_label(target)
  end,
  function()
    T.check("jumped to epsilon", table.concat({ serpis.buffer.cursor() }, ":"), "2:7")
    T.check("labels gone", #serpis.jump.labels(), 0)
    T.press("M-g")
  end,
  function() T.press("w") end,
  function() T.press("ESC") end,
  function()
    T.check("ESC cancels", #serpis.jump.labels(), 0)
    T.check("cursor stays after cancel", table.concat({ serpis.buffer.cursor() }, ":"), "2:7")
    -- ninja: a single URL and a single hash are copied at once; the cursor doesn't move.
    T.press("s-g u")
  end,
  function()
    T.check("ninja copies the URL", last_message(), "Copied: https://example.com/x")
    T.press("s-g h")
  end,
  function()
    T.check("ninja copies the hash", last_message(), "Copied: 4c898c3")
    T.check("copying leaves the cursor", table.concat({ serpis.buffer.cursor() }, ":"), "2:7")
    T.press("s-g q")
  end,
  function()
    T.check("nothing quoted: a message, no labels", #serpis.jump.labels() == 0 and last_message():find("Nothing") ~= nil, true)
    -- Two windows: labels in both.
    serpis.window.split("right")
    other = serpis.buffer.create("text", "*jump-b*", { text = "omega psi chi" })
    serpis.buffer.show(other)
  end,
  function() T.press("M-g") end,
  function() T.press("w") end,
  function()
    local in_a = label_for(function(l) return l.buffer == text end)
    target = label_for(function(l) return l.buffer == text and l.line == 1 and l.col == 7 end)
    T.check("labels in the other window too", in_a ~= nil, true)
    press_label(target)
  end,
  function()
    T.check("jump focuses the other window", serpis.buffer.current(), text)
    T.check("at beta", table.concat({ serpis.buffer.cursor() }, ":"), "1:7")
    serpis.window.only()
    -- vim: gw.
    serpis.keys.scheme("vim")
  end,
  function() T.press("g w") end,
  function()
    target = label_for(function(l) return l.buffer == text and l.line == 3 and l.col == 1 end)
    T.check("gw labels words", target ~= nil, true)
    press_label(target)
  end,
  function()
    T.check("gw jumped", table.concat({ serpis.buffer.cursor() }, ":"), "3:1")
    -- A chat: its composer is a target too.
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
  end,
  function() d.text("find the needle here") end,
  function() T.press("ESC") end,
  function() T.press("g w") end,
  function()
    target = label_for(function(l) return l.buffer == chat and l.part == 1 and l.col == 10 end)
    T.check("composer words labelled", target ~= nil, true)
    press_label(target)
  end,
  function()
    local st = d.composer_state()
    T.check("jumped inside the composer", st.focus == "prompt" and st.line == 1 and st.col == 10, true)
    -- A terminal: copy a URL from its output.
    serpis.command.run("new-terminal")
    term = serpis.buffer.current()
    return 900
  end,
  function()
    d.text("echo https://serpis.dev/term-url")
    T.press("RET")
    return 800
  end,
  function() T.press("C-c ESC") end,
  function() T.press("s-g u") end,
  function()
    -- The command line and its output both hold the URL: two labels (or one copy).
    local ls = serpis.jump.labels()
    if #ls > 0 then press_label(ls[1]) end
  end,
  function()
    T.check("ninja copies from terminal output", last_message(), "Copied: https://serpis.dev/term-url")
  end,
})
