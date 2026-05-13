# [Project Name]: Design One-Pager (GreenDoc)

**Author(s):** [Name]  
**Status:** [Draft / Approved / Shipped]  
**Last Updated:** YYYY-MM-DD  
**Estimated Effort:** [e.g., 2 weeks, 1 sprint]

---

## 1. Summary
A 2–3 sentence overview of the change. What are you doing and why? 

## 2. Problem Statement
Describe the specific pain point or "broken" state this project addresses.
*   *Example: Currently, users cannot filter their search history by date, leading to high latency in manual lookup.*

## 3. Proposed Solution
Explain the high-level logic of the fix or feature. 
*   What is the specific code change or configuration update?
*   How does it interact with existing systems?
*   *Note: Use a single simple diagram if the logic is non-trivial.*

## 4. Risks & Trade-offs
Even small changes have risks. Address them upfront.
*   **Performance:** Will this increase memory usage?
*   **Complexity:** Does this add a new dependency?
*   **Backwards Compatibility:** Will this break existing clients?
*   **Alternatives:** Why did you choose this over a "quicker" or "better" fix?

## 5. Success Criteria (Metrics)
How will you know this worked?
*   [ ] Primary metric (e.g., "Feature usage > 5%")
*   [ ] Guardrail metric (e.g., "Latency does not increase by > 10ms")

## 6. Implementation & Rollout
A brief bulleted list of the steps to ship.
1.  Feature flag implementation.
2.  Canary to 1% of users.
3.  Full rollout.

---

### Comparison: BlueDoc vs. GreenDoc

| Feature | BlueDoc (Standard) | GreenDoc (One-Pager) |
| :--- | :--- | :--- |
| **Scope** | Major systems, new services. | Small features, optimizations, bug fixes. |
| **Length** | 5–20+ pages. | 1–2 pages. |
| **Review** | Cross-functional committees (SRE, Security). | Peer-level or Team Lead review. |
| **Focus** | Long-term scalability and architecture. | Immediate impact and implementation. |

### Markdown Tips for GreenDocs:
*   **Be Brutally Concise:** If a section requires more than three paragraphs, consider upgrading to a **BlueDoc**.
*   **Checklists:** Use `[ ]` to show remaining work or requirements.
*   **Inline Links:** Link directly to the relevant code files or bug tracker (Jira/Buganizer) to keep the doc self-contained.

