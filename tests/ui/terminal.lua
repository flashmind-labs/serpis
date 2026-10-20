local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")
serpis.command.run("new-terminal")

-- Run steps one after another, each `ms` after the previous, so the shell can echo.
local function steps(ms, list)
  local function run(i)
    if i > #list then return end
    serpis.defer(ms, function()
      list[i]()
      run(i + 1)
    end)
  end
  run(1)
end

-- Call `f` once `ready()` holds (polled every 100ms, up to 15s).
local function when(ready, f, tries)
  tries = tries or 150
  if ready() or tries == 0 then return f() end
  serpis.defer(100, function() when(ready, f, tries - 1) end)
end

-- The command line: the cursor's line from the first "echo"/"printf" (cuts off the prompt).
local function cmdline()
  local l = serpis.buffer.line(serpis.buffer.cursor())
  return l:match("echo.*$") or l:match("printf.*$") or l
end

-- Unique words, so history autosuggestions can't complete them.
local tag = tostring(os.time())
local a, b, c = "qa" .. tag, "qb" .. tag, "qc" .. tag

-- Wait for the prompt, and a little longer for the line editor to start reading keys.
when(function() return serpis.buffer.text():find("%S") ~= nil end, function() serpis.defer(700, function()
  serpis.buffer.insert("echo serpis-term-marker\r")
  steps(900, {
    function()
      T.check("starts in insert", serpis.buffer.modes()[1], "vim-insert")
      -- Normal state = copy mode over the scrollback: search the output and yank the word.
      serpis.keys.feed("ESC")
      T.check("ESC enters normal", serpis.buffer.modes()[1], "vim-normal")
      serpis.keys.feed("g g")
      T.check("scrollback has output", serpis.buffer.text():find("serpis%-term%-marker") ~= nil, true)
      serpis.keys.feed("i")
      T.check("i returns to insert", serpis.buffer.modes()[1], "vim-insert")
      serpis.buffer.insert("echo " .. a .. " " .. b .. " " .. c)
    end,
    function()
      -- Normal state on the command line edits it through the shell.
      serpis.keys.feed("ESC b d w")
      T.check("dw deletes the last word", cmdline(), "echo " .. a .. " " .. b)
      serpis.keys.feed("^")
      local l, col = serpis.buffer.cursor()
      local line = serpis.buffer.line(l)
      T.check("^ lands past the prompt", line:sub(utf8.offset(line, col), utf8.offset(line, col)), "e")
      serpis.keys.feed("c w")
      T.check("cw enters insert", serpis.buffer.modes()[1], "vim-insert")
      serpis.buffer.insert("printf")
    end,
    function()
      T.check("^ cw changes the first word", cmdline(), "printf " .. a .. " " .. b)
      serpis.keys.feed("ESC w x")
      T.check("x deletes a char", cmdline(), "printf " .. a:sub(2) .. " " .. b)
      serpis.keys.feed("$ P")
      T.check("P pastes the deleted char", cmdline(), "printf " .. a:sub(2) .. " " .. b:sub(1, -2) .. "q" .. b:sub(-1))
      serpis.keys.feed("d d")
      T.check("dd clears the line, keeps the prompt", serpis.buffer.line(serpis.buffer.cursor()):find("printf") == nil and serpis.buffer.line(serpis.buffer.cursor()) ~= "", true)
      serpis.keys.feed("A")
      serpis.buffer.insert("echo done")
    end,
    function()
      T.check("A appends", cmdline(), "echo done")
      T.finish()
    end,
  })
end) end)
