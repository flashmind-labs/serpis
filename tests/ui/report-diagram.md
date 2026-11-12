Here is how a review comment travels, from where you write it to the agent's answer.

The pieces, and what talks to what:

```mermaid
flowchart LR
    subgraph Editor["serpis (editor)"]
        UI[Chat view] --> Chat[Chat buffer]
        Chat --> Review[(Review store)]
    end
    subgraph Agent["Agent process"]
        Loop{Run loop} -->|tool call| Tools[Tools: bash, edit, read]
        Tools -->|result| Loop
    end
    Chat -- prompt --> Loop
    Loop -. streamed events .-> Chat
```

What happens when you send the review:

```mermaid
sequenceDiagram
    actor U as You
    participant C as Chat buffer
    participant A as Agent
    U->>C: Select text, press c
    C->>C: Save the comment
    U->>+C: Send
    C->>+A: Review + "Also"
    A-->>-C: Answer, quoting each comment
    C-->>-U: Comments marked answered
```

And the states a comment goes through:

```mermaid
stateDiagram-v2
    [*] --> Writing: press c
    Writing --> Saved: RET
    Writing --> [*]: ESC
    Saved --> Writing: click it
    Saved --> Sent: send the review
    Sent --> Answered: the agent quotes it
    Answered --> [*]
```

A diagram serpis can't draw stays as its source:

```mermaid
this is not a diagram
```
