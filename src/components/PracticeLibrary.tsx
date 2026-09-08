import './PracticeLibrary.css';
import { memo, useState } from 'react';

const categories = ['All', 'Introduction', 'Behavioral', 'Technical', 'Leadership'] as const;
type Category = typeof categories[number];
const questions: { category: Exclude<Category, 'All'>; question: string; hint: string }[] = [
  { category: 'Introduction', question: 'Tell me about yourself.', hint: 'Connect your current work, one relevant achievement, and what you want to do next.' },
  { category: 'Introduction', question: 'Why are you interested in this role?', hint: 'Match two responsibilities in the job description to experience you can explain.' },
  { category: 'Introduction', question: 'What is a strength you would bring to this team?', hint: 'Choose a strength and support it with a specific example and outcome.' },
  { category: 'Behavioral', question: 'Tell me about a difficult problem you solved.', hint: 'Explain the situation, your responsibility, your actions, and the result.' },
  { category: 'Behavioral', question: 'Describe a disagreement with a teammate and how you handled it.', hint: 'Show how you listened, compared options, and reached a workable decision.' },
  { category: 'Behavioral', question: 'Tell me about a mistake and what you learned from it.', hint: 'Own your part, explain the repair, and name the habit you changed.' },
  { category: 'Technical', question: 'Walk me through the architecture of a project you built.', hint: 'Start with users and constraints, then describe components, data flow, and tradeoffs.' },
  { category: 'Technical', question: 'How would you investigate a slow application?', hint: 'Reproduce the issue, measure the bottleneck, test one change, and compare results.' },
  { category: 'Technical', question: 'How do you decide what to test before shipping a change?', hint: 'Discuss user impact, failure modes, boundaries, and the checks that give confidence.' },
  { category: 'Leadership', question: 'How do you prioritize when several deadlines compete?', hint: 'Compare impact and urgency, explain tradeoffs, and communicate what will move.' },
  { category: 'Leadership', question: 'Tell me about a time you helped someone grow.', hint: 'Describe their goal, the support you gave, and how you knew it helped.' },
  { category: 'Leadership', question: 'Describe a decision you made with incomplete information.', hint: 'Name the uncertainty, the evidence you had, and how you limited risk and followed up.' },
];

export default memo(function PracticeLibrary({ disabled, onChoose }: {
  disabled: boolean;
  onChoose(question: string): void;
}) {
  const [category, setCategory] = useState<Category>('All');
  const [search, setSearch] = useState('');
  const query = search.trim().toLowerCase();
  const visible = questions.filter((item) =>
    (category === 'All' || item.category === category) &&
    (item.question + ' ' + item.hint).toLowerCase().includes(query)
  );

  return (
    <section className="prep-panel" aria-label="Interview preparation">
      <div className="prep-heading">
        <h2>Make your next answer clearer</h2>
        <p>Pick a question, make it your own, then press Ask. These guides are available offline.</p>
      </div>
      <div className="prep-filters">
        <label className="sr-only" htmlFor="prep-search">Search practice questions</label>
        <input id="prep-search" type="search" placeholder="Search questions…" value={search}
          onChange={(event) => setSearch(event.target.value)} />
        <label className="sr-only" htmlFor="prep-category">Question category</label>
        <select id="prep-category" value={category} onChange={(event) => setCategory(event.target.value as Category)}>
          {categories.map((item) => <option key={item}>{item}</option>)}
        </select>
      </div>
      <p className="prep-count" role="status">{visible.length} of {questions.length} questions</p>
      {disabled && <p className="field-help">You can browse now. Choose a question when recording or sending has finished.</p>}
      <div className="prep-questions">
        {visible.map((item) => (
          <button key={item.question} type="button" className="prep-question" disabled={disabled}
            onClick={() => onChoose(item.question)}>
            <span className="prep-category">{item.category}</span>
            <strong>{item.question}</strong>
            <span>{item.hint}</span>
          </button>
        ))}
        {visible.length === 0 && (
          <div className="prep-empty">
            <p>No matching questions. Try a broader search or another category.</p>
            <button type="button" className="ghost-button" onClick={() => { setSearch(''); setCategory('All'); }}>
              Reset filters
            </button>
          </div>
        )}
      </div>
      <details className="prep-guide">
        <summary>Three answer frameworks</summary>
        <dl>
          <dt>Behavioral: situation → task → action → result</dt>
          <dd>Give just enough context, state your responsibility, focus on what you did, and finish with the outcome and lesson.</dd>
          <dt>Technical: clarify → approach → tradeoffs → verify</dt>
          <dd>Confirm constraints, outline your solution, compare alternatives, and explain how you would test it.</dd>
          <dt>Introduction: present → past → next</dt>
          <dd>Say what you do now, connect a relevant past achievement, and explain why this opportunity fits your next step.</dd>
        </dl>
        <p>Use your own experience. Include numbers only when you can support them.</p>
      </details>
      <details className="prep-guide">
        <summary>Before the call</summary>
        <ul>
          <li>In Settings, add an answer-provider key. Add a Deepgram key for recorded questions.</li>
          <li>Save your resume and this role’s job description so answers have relevant context.</li>
          <li>Play a short audio sample and check that Record produces a transcript. Capture uses system audio, not your microphone.</li>
          <li>Try a typed question and check that an answer arrives before your call starts.</li>
          <li>Keep two or three real project examples ready, including your contribution and the outcome.</li>
        </ul>
      </details>
      <details className="prep-guide">
        <summary>Questions to ask the interviewer</summary>
        <ul>
          <li>What would success look like in the first three months?</li>
          <li>What is the biggest challenge the team wants this hire to help solve?</li>
          <li>How does the team give feedback and make decisions?</li>
          <li>What are the next steps in the interview process?</li>
        </ul>
      </details>
    </section>
  );
});
