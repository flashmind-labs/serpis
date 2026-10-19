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

function T.finish()
  local f = io.open(os.getenv("SERPIS_TEST_OUT"), "w")
  f:write(table.concat(T.results, "\n"), "\n")
  f:close()
  serpis.debug.quit()
end

return T
