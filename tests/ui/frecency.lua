-- Frecency in prompts: what you pick often and recently ranks first. M-x lists it first even
-- before typing; while typing it breaks ties; buffer switching keeps the most recent buffer first
-- with nothing typed. Scores persist in state.json.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
for _, n in ipairs({ "test-alpha", "test-zeta", "zz-last" }) do
  serpis.command.define(n, function() end, "A test command.")
end

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    list[i]()
    serpis.defer(200, go)
  end
  go()
end
local function first() local c = d.candidates()[1]; return c and c.label end
-- Run a command through M-x by typing its name.
local function mx(name) return {
  function() serpis.command.run("execute-extended-command") end,
  function() d.text(name) end,
  function() d.key("Enter") end,
} end

local list = {
  function() serpis.command.run("execute-extended-command") end,
  function()
    T.check("M-x starts alphabetical", first() ~= "zz-last", true)
    d.key("Escape")
  end,
}
for _, s in ipairs(mx("zz-last")) do table.insert(list, s) end
for _, s in ipairs(mx("zz-last")) do table.insert(list, s) end
for _, s in ipairs(mx("test-zeta")) do table.insert(list, s) end
for _, s in ipairs({
  function() serpis.command.run("execute-extended-command") end,
  function()
    T.check("what you use most comes first before typing", first(), "zz-last")
    d.text("test-")
  end,
  function()
    T.check("and wins ties while typing", first(), "test-zeta")
    d.key("Escape")
    -- Buffers: pick *scratch* repeatedly, then visit another; with nothing typed the most
    -- recent other buffer still comes first.
    local a = serpis.buffer.create("text", "*fa*", { text = "" })
    local b = serpis.buffer.create("text", "*fb*", { text = "" })
    serpis.buffer.show(a)
    serpis.buffer.show(b)
  end,
  function() serpis.command.run("switch-to-buffer") end,
  function()
    T.check("buffer switching offers the most recent buffer first", first(), "Fa")
    d.key("Escape")
  end,
  function()
    local f = io.open(serpis.config_dir .. "/state.json")
    local s = f and f:read("*a") or ""
    if f then f:close() end
    T.check("scores are saved", s:find('"zz-last"', 1, true) ~= nil, true)
  end,
}) do table.insert(list, s) end
steps(list)
