-- How a report is drawn: one written to the report skill as its parts (outcome badge, what
-- needs you, the headline, Needs you as a card, checks marked), on a surface of its own; a long
-- ending that isn't in the skill's shape as plain markdown on that surface; a short "done" as
-- it always was.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)

-- The drawn lines from the first one whose text has `from` on.
local function lines_from(from)
  local all, out, on = d.chat_lines() or {}, {}, false
  for _, l in ipairs(all) do
    if not on and l.text:find(from, 1, true) then on = true end
    if on then out[#out + 1] = l end
  end
  return out
end
local function first(lines, pred)
  for _, l in ipairs(lines) do if pred(l) then return l end end
end
local function count(lines, pred)
  local n = 0
  for _, l in ipairs(lines) do if pred(l) then n = n + 1 end end
  return n
end
local yellow = serpis.theme.colors and serpis.theme.colors().yellow

local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "STEPS:Run the tests|echo ok"); return 2500 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "REPORT:skill"); return 2500 end,
  function()
    local all = d.chat_lines()
    local done = first(all, function(l) return l.text == "done" end)
    T.check("a short ending stays as it was", done and done.surface, false)

    local r = lines_from("Report  ·  m  ·  301 words")
    local head = r[1]
    T.check("the report's header has its outcome", head and head.text:sub(1, 6), " Done ")
    T.check("and how long it is", head and head.text:find("301 words, about 2 min", 1, true) ~= nil, true)
    T.check("and what needs you", head and head.text:find("3 need you", 1, true) ~= nil, true)
    T.check("its first sentence is the headline", r[2] and r[2].text, "The retry change is sound, but retry_total is now counted twice and needs a fix before merging.")
    T.check("its sections are counted", first(r, function(l) return l.text == "Needs you   3" end) ~= nil, true)
    T.check("all of it on its surface", count(r, function(l) return not l.surface end), 0)
    local items = {}
    for _, l in ipairs(r) do if l.marker and l.marker:match("^%d%.$") and l.bar then items[#items + 1] = l end end
    T.check("Needs you is a card, its 3 items in it", #items, 3)
    if yellow then T.check("with the warning color's bar", items[1] and items[1].bar, yellow) end
    T.check("checks that passed are marked", count(r, function(l) return l.marker == "✓" end), 3)
    T.check("what wasn't checked is marked", count(r, function(l) return l.marker == "○" end), 1)
    local link = first(r, function(l) return l.text:find("src/queue.rs:142", 1, true) end)
    T.check("its places in the code are links", link and link.links[1], "src/queue.rs:142")
    serpis.chat.prompt(serpis.buffer.current(), "REPORT")
    return 2500
  end,
  function()
    local r = lines_from("Report  ·  m  ·  385 words")
    local head = r[1]
    T.check("a long ending not in the skill's shape is a report too", head ~= nil, true)
    T.check("without an outcome", head and head.text:sub(1, 6) == " Done ", false)
    T.check("or a count of what needs you", head and head.text:find("need you", 1, true) == nil, true)
    T.check("its headings as written", first(r, function(l) return l.text == "Things to consider" end) ~= nil, true)
    T.check("on its surface too", count(r, function(l) return not l.surface end), 0)
    T.check("nothing marked as checked", count(r, function(l) return l.marker == "✓" or l.marker == "○" end), 0)
    T.finish()
    return 100000
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
