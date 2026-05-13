# [Project Name]: Design Doc

**Authors:** [Author Name(s)]  
**Status:** [Draft / In-Review / Approved / Obsolete]  
**Last Updated:** YYYY-MM-DD  
**Reviewers:** [List of reviewers, usually @usernames]  
**Approvers:** [List of final decision makers]  
**Document Link:** [Link to this file or rendered version]

---

## 1. Abstract
A high-level summary (1–3 paragraphs) of what the project is, what problem it solves, and the proposed solution. This should be readable by a non-expert.

## 2. Background
Context for why this project exists. 
- What is the current state?
- What are the pain points? 
- Are there existing systems that this will replace or interact with? 
- Include links to relevant PRDs (Product Requirement Documents) or previous design docs.

## 3. Goals & Non-Goals
Clarity on scope is critical to prevent scope creep.

### 3.1. Goals
*   **Primary Goal:** The most important outcome.
*   Metric-driven goals (e.g., "Reduce latency by 20%").
*   Functional requirements (e.g., "Allow users to edit comments").

### 3.2. Non-Goals
*   Features that might seem related but are explicitly out of scope.
*   Future improvements that are deferred.

## 4. Proposed Design
The "meat" of the document. Start with the high-level architecture and zoom in.

### 4.1. High-Level Architecture
Provide a high-level diagram or description of how the system fits together. 
> *Tip: Use Mermaid.js or link to an embedded image.*

### 4.2. Detailed Design
Go into specific components, APIs, and data models.
*   **API Definitions:** Describe new endpoints, Protobuf definitions, or CLI commands.
*   **Data Schema:** Database tables, key-value structures, or file formats.
*   **Workflows:** Step-by-step logic for complex operations (e.g., auth flow).

## 5. Cross-Cutting Concerns
Google design docs place heavy emphasis on these "standard" reviews.

### 5.1. Security & Privacy
- How is data encrypted?
- What are the access control lists (ACLs)?
- Does this handle PII (Personally Identifiable Information)?

### 5.2. Observability (Monitoring & Logging)
- What metrics will be exported (e.g., RPC error rates, latency)?
- What logging is required for debugging?
- What are the "Golden Signals" for the dashboard?

### 5.3. Scalability & Performance
- What are the expected QPS (Queries Per Second)?
- How does the system scale (Horizontal vs. Vertical)?
- What are the resource requirements (CPU, RAM, Storage)?

### 5.4. Testing Plan
- Unit tests, integration tests, and end-to-end tests.
- Strategy for load testing or "chaos" testing.

## 6. Alternatives Considered
*A BlueDoc is not just about the chosen path, but why others were rejected.*
*   **Alternative A:** Briefly describe it and why it was rejected (e.g., "Too complex," "High latency").
*   **Alternative B:** Why "Doing Nothing" is not an option.

## 7. Implementation Plan
- **Phase 1:** Minimum Viable Product (MVP).
- **Phase 2:** Feature parity or migrations.
- **Rollout/Rollback:** How will the feature be toggled? (e.g., feature flags).

## 8. Glossary / References
- Links to external libraries.
- Definitions for project-specific acronyms.

---

### Markdown Style Tips (Google Conventions):
1.  **Line Length:** Google's internal style guide suggests a soft limit of **80 characters** per line for source Markdown to make it easier to review in code-diff tools.
2.  **Headings:** Use `#` for title, `##` for sections, and `###` for subsections.
3.  **TOC:** If your environment supports it, use `[TOC]` at the top to generate a Table of Contents.
4.  **Diagrams:** Use **Mermaid** blocks if using GitHub/GitLab, otherwise link to a stable SVG/PNG.
    ```mermaid
    graph TD;
      A-->B;
      A-->C;
      B-->D;
      C-->D;
    ```
