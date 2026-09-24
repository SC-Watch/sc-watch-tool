/*
 sc-watch - reads player handles off the Star Citizen HUD.
 Copyright (C) 2026 SC-Watch

 This program is free software: you can redistribute it and/or modify it
 under the terms of the GNU General Public License as published by the Free
 Software Foundation, either version 3 of the License, or (at your option)
 any later version.

 This program is distributed in the hope that it will be useful, but WITHOUT
 ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 more details. You should have received a copy of the GNU General Public
 License along with this program. If not, see <https://www.gnu.org/licenses/>.
*/
const grid=document.getElementById('grid'), empty=document.getElementById('empty');
const q=document.getElementById('q'), fAlerts=document.getElementById('f-alerts');
const fSettings=document.getElementById('f-settings');
const setPane=document.getElementById('settings'), setNav=document.getElementById('setnav'),
      setPanel=document.getElementById('setpanel');
let data={contacts:[],stats:{}};
let settingsOpen=false, settingsCat='capture';
let lookupOpen=false;

fAlerts.onclick=()=>{fAlerts.setAttribute('aria-pressed',
  fAlerts.getAttribute('aria-pressed')==='true'?'false':'true'); render();};
q.oninput=render;

document.getElementById('f-lookup').onclick=openLookup;

fSettings.onclick=()=>{
  settingsOpen=!settingsOpen;
  if(settingsOpen && orgsOpen){ orgsOpen=false; fOrgs.setAttribute('aria-pressed','false'); }
  fSettings.setAttribute('aria-pressed', settingsOpen?'true':'false');
  render();
  if(settingsOpen) renderSettings();
};

const fOrgs=document.getElementById('f-orgs');
const orgPane=document.getElementById('orgs'), orgNavList=document.getElementById('orgnavlist'),
      orgNavAdd=document.getElementById('orgnavadd'), orgHead=document.getElementById('orghead'),
      orgMemAdd=document.getElementById('orgmemadd'), orgMembers=document.getElementById('orgmembers');
let orgMemAddFor=null, orgArmedMember=null;
let orgsOpen=false, orgSel=null, orgArmed=null;
const orgData={};                 // sid -> /api/org/members payload
let orgFetchAt=0, orgFetchTimer=null;

function openOrgs(sid){
  orgsOpen=true;
  if(settingsOpen){ settingsOpen=false; fSettings.setAttribute('aria-pressed','false'); }
  fOrgs.setAttribute('aria-pressed','true');
  if(sid) orgSel=sid;
  orgNavAdd.innerHTML=orgAddHTML();
  render();
  loadMembers(true);
}
fOrgs.onclick=()=>{
  if(orgsOpen){ orgsOpen=false; fOrgs.setAttribute('aria-pressed','false'); render(); }
  else openOrgs();
};

function ago(ts){
  if(!ts) return '–';
  const s=Math.max(0,Date.now()/1000-ts);
  if(s<60) return Math.round(s)+'s ago';
  if(s<3600) return Math.round(s/60)+'m ago';
  if(s<86400) return Math.round(s/3600)+'h ago';
  return Math.round(s/86400)+'d ago';
}
function esc(s){return String(s??'').replace(/[&<>"]/g,
  c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));}
// Anything going into an href. esc() stops a string breaking OUT of the
// attribute; it does nothing about what the URL then does, and a
// javascript: link in this page runs in the origin that can drive every
// write endpoint. Watchlist links come from a file somebody else wrote.
function safeURL(u){ return /^https?:\/\//i.test(String(u||'')) ? u : ''; }
// Tint digits so 5LIVER cannot be mistaken for SLIVER at a glance.
// Tinting digits answers a live question: "could this 5 actually be an S?"
// Once the exact string has resolved to a real RSI account, that question is
// settled - FROST1545 is verifiably FROST1545 - and marking the digits after
// that is stale information dressed as a warning. So the tint is for
// UNVERIFIED handles only, which is precisely where a 5/S mix-up would do
// damage. A handle the API says does not exist keeps the tint emphatically:
// that is the case most likely to be a misread.
function handleHTML(s, verified){
  const t=esc(s);
  return verified ? t : t.replace(/[0-9]/g,d=>`<span class="d">${d}</span>`);
}

// A forced, fresh lookup of this contact, run IN THE BACKGROUND with its
// status on the tile. It used to open the lookup window and make you wait in
// it - up to 50 s, measured, while the older service was timing out - for an
// answer the tile then showed anyway. The full message lands in the footer.
// Offered wherever the tile has no current profile to show.
function lookupBtn(c, label){
  return `<button class="lookupbtn" data-handle="${esc(c.handle)}"
    title="ask RSI now, skipping the cache - runs in the background">${label}</button>`;
}

// handle -> {busy, done, t0} while running; {msg, kind, until} after.
const tileLookups={};
let tileLookupTimer=null;
const TILE_LOOKUP_SHOW=12000, TILE_LOOKUP_GIVEUP=150000;

function lookupStatusHTML(c, label){
  const s=tileLookups[c.handle];
  if(s&&s.busy) return `<span class="lk-status busy">looking up…</span>`;
  const note=s&&s.msg&&Date.now()<s.until
    ? ` <span class="lk-status ${s.kind}">${esc(s.msg)}</span>`:'';
  return lookupBtn(c,label)+note;
}

async function startTileLookup(handle){
  if(tileLookups[handle]&&tileLookups[handle].busy) return;
  tileLookups[handle]={busy:true, done:Infinity, t0:Date.now()};
  render();
  try{
    const j=await post('/api/lookup/background',{handle});
    // "done" is how many lookups of this handle had finished when ours was
    // asked for; ours is finished when the server's count passes it.
    tileLookups[handle].done=j.done;
  }catch(e){
    tileLookups[handle]={msg:'could not start: '+e.message, kind:'bad',
                         until:Date.now()+TILE_LOOKUP_SHOW};
    render(); return;
  }
  // A lookup that changes nothing writes nothing, and the stream only speaks
  // when something is written - so while one runs, ask.
  if(!tileLookupTimer) tileLookupTimer=setInterval(()=>{
    if(Object.values(tileLookups).some(s=>s.busy)) poll();
    else{ clearInterval(tileLookupTimer); tileLookupTimer=null; }
  }, 2500);
}

// Called on every render: has any running lookup finished?
function settleTileLookups(){
  const done=data.stats.lookup_done||{}, msgs=data.stats.lookups||{};
  for(const [h,s] of Object.entries(tileLookups)){
    if(!s.busy) continue;
    const key=h.toUpperCase();
    if((done[key]||0) > s.done){
      const full=msgs[key]||'';
      const kind=/lookup failed/.test(full)?'bad'
                :/no profile|incomplete/.test(full)?'warn':'ok';
      // Short: the tile's own line already gives the reason, and the footer
      // has the whole message.
      tileLookups[h]={kind, until:Date.now()+TILE_LOOKUP_SHOW,
        msg: kind==='bad' ? 'failed'
           : /no profile/.test(full) ? 'no RSI account'
           : /incomplete/.test(full) ? 'updated, partly' : 'updated'};
      say(full);
      setTimeout(render, TILE_LOOKUP_SHOW+200);
    }else if(Date.now()-s.t0 > TILE_LOOKUP_GIVEUP){
      tileLookups[h]={msg:'no answer yet - try again later', kind:'warn',
                      until:Date.now()+TILE_LOOKUP_SHOW};
    }
  }
}

// A stored profile past its expiry. Still shown - orgs and account age change
// slowly, and on a day the lookup service is down it is the only answer there
// is - but never passed off as current.
function staleHTML(c){
  if(!c.known_profile || !c.profile_stale) return '';
  const age=c.profile_age_days!=null ? c.profile_age_days+' days ago' : 'a while ago';
  // A partial profile says why, with its own button (partialHTML): only its
  // age here, or the same reason and a second button would sit one under the
  // other.
  if(c.profile_partial) return `<div class="noorg stale-profile">profile from ${age}</div>`;
  const why=c.lookup_failed
    ? `could not refresh: ${esc(c.lookup_failed.error)}`
    : 'due for a refresh';
  return `<div class="noorg stale-profile">profile from ${age} — ${why}
    ${lookupStatusHTML(c,'refresh')}</div>`;
}

// An org's declared focus, or a plain statement that nobody here could get
// it. Blank would read as "nothing to see", which is the one thing it is not.
function focusHTML(o, edit){
  if(edit && o.focus_unknown)
    return `<button class="mini unknown" data-orgedit="${esc(o.sid)}"
      title="the lookup could not get this — add it from the org's RSI page"
      >focus unknown — add</button>`;
  if(o.focus_unknown) return '<span class="focus unknown">focus unknown</span>';
  if(o.focus_manual)
    return `<span class="focus yours" title="your entry, not RSI's">${
      esc(o.focuses.join(' / '))} · yours${edit?` <button class="mini"
      data-orgedit="${esc(o.sid)}" title="change or clear your entry">edit</button>`:''}</span>`;
  return `<span class="focus">${esc(o.focuses.join(' / '))}</span>`;
}

// Main org first, affiliates hung under it with a small label - they used to
// be rows identical to the main one, told apart only by "affil" in the grey
// tag line. A player can have nine; on a TILE, plain ones past AFFIL_SHOWN
// fold into "+N more" (the detail window lists them all), but one with a
// piracy or adjacent focus, or a focus nobody could get, is never folded.
const AFFIL_SHOWN=3;
function orgRowHTML(o, edit){
  const editing=edit&&modalEdit&&modalEdit.kind==='org'&&modalEdit.sid===o.sid;
  return `<div class="org${o.is_main?'':' affil'}"><span>
      <a class="org-name${o.name_manual?' yours':''}" href="${esc(o.url)}" target="_blank"
         rel="noopener" ${o.name_manual?'title="name: your entry"':''}>${esc(o.name||o.sid||'—')}</a>
      ${o.name||o.members?`<span class="org-tag">${esc(o.sid)}${
        o.members?` · ${o.members} members`:''}</span>`:''}
    </span>${focusHTML(o, edit)}</div>${editing?orgEditorHTML(o):''}`;
}
function orgsHTML(orgs, fold, edit){
  const main=orgs.filter(o=>o.is_main), aff=orgs.filter(o=>!o.is_main);
  const loud=o=>o.piracy||(o.adjacent||[]).length||o.focus_unknown;
  let plain=0;
  const shown=fold ? aff.filter(o=>loud(o)||plain++<AFFIL_SHOWN) : aff;
  const more=aff.length-shown.length;
  const row=o=>orgRowHTML(o, edit);
  return main.map(row).join('')+(aff.length?`
    <div class="affils">
      <div class="affil-label">affiliate${aff.length===1?'':'s'}</div>
      ${shown.map(row).join('')}
      ${more?`<div class="affil-more">+${more} more — open the contact for all</div>`:''}
    </div>`:'');
}

// ---- your own entries, in the detail window --------------------------------
// Where a lookup could not get something - an org's focus or name, when an
// account was made - the detail window takes it from you, typed off their RSI
// page. Only there; the tile just shows the result, marked as yours. An entry
// fills a gap and never overrides: once a lookup brings the real value, that
// is what shows. The editor's state lives here rather than in the DOM, because
// the window is redrawn whenever the data changes and an open editor would
// otherwise vanish halfway through a choice.
let modalEdit=null;   // {kind:'org', sid, vals} | {kind:'enlisted', vals}

function focusOptions(sel, blank){
  return `<option value="">${blank}</option>`+(data.stats.focuses||[]).map(f=>
    `<option value="${esc(f)}"${f===sel?' selected':''}>${esc(f)}</option>`).join('');
}
function orgEditorHTML(o){
  const v=modalEdit.vals;
  const prim=v.primary??(o.focus_manual?o.focuses[0]||'':'');
  const sec=v.secondary??(o.focus_manual?o.focuses[1]||'':'');
  const name=v.name??(o.name_manual?o.name:'');
  return `<div class="medit">
    <select class="me-f" data-k="primary">${focusOptions(prim,'primary focus…')}</select>
    <select class="me-f" data-k="secondary">${focusOptions(sec,'no secondary focus')}</select>
    ${!o.name||o.name_manual?`<input class="me-f" data-k="name" maxlength="80"
       placeholder="org name (optional)" value="${esc(name)}">`:''}
    <button class="save me-save">save</button>
    ${o.focus_manual||o.name_manual?'<button class="me-clear">clear</button>':''}
    <button class="cancel me-cancel">cancel</button>
    <div class="blurb">From the org's page on RSI. Applies to everyone in
      ${esc(o.sid)}, and gives way to RSI's own answer once a lookup gets it.</div>
  </div>`;
}
function enlistedHTML(c){
  const age=d=>d<365?d+'d':Math.floor(d/365)+'y';
  if(c.enlisted_days!=null)
    return ` · enlisted ${age(c.enlisted_days)}${c.enlisted_manual
      ?` <span class="yours" title="your entry, not RSI's">(yours)</span>
         <button class="mini" data-edit="enlisted">edit</button>`:''}`;
  if(c.enlisted_missing)
    return ` · <button class="mini unknown" data-edit="enlisted"
      title="the lookup did not bring this — add it from their RSI page"
      >account age unknown — add</button>`;
  return '';
}
function enlistedEditorHTML(c){
  const v=modalEdit.vals.enlisted??(c.enlisted_iso||'');
  const today=new Date().toISOString().slice(0,10);
  return `<div class="medit">
    <label>enlisted <input type="date" class="me-f" data-k="enlisted"
      min="2011-01-01" max="${today}" value="${esc(v)}"></label>
    <button class="save me-save">save</button>
    ${c.enlisted_manual?'<button class="me-clear">clear</button>':''}
    <button class="cancel me-cancel">cancel</button>
    <div class="blurb">The "Enlisted" date on their RSI profile page. Gives way
      to RSI's own answer once a lookup gets it.</div>
  </div>`;
}
// Putting this player on a flagged org's roster by hand - for the people a
// public roster does not show. The same records the Orgs tab adds to.
function rosterSectionHTML(c){
  const fl=data.stats.flagged_orgs_list||[];
  if(!fl.length) return '';
  const on=new Set(c.flagged_sids||[]), mine=c.manual_sids||[];
  const by=c.manual_by||{};      // sid -> the org mate who added them; absent = you
  const addable=fl.filter(o=>!on.has(o.sid));
  return `<section><h3>flagged org rosters</h3>
    ${mine.map(sid=>`<div class="rep">
      <span>on <b>${esc(sid)}</b>'s roster — added by ${esc(by[sid]||'you')}</span>
      <span style="flex:1"></span>
      <button class="del ro-rm ${armedDel==='ro:'+sid?'armed':''}"
        data-rosid="${esc(sid)}">${armedDel==='ro:'+sid
          ?'take them off? click again':'remove'}</button></div>`).join('')}
    ${addable.length?`<div class="rep">
      <select class="ro-sid">${addable.map(o=>`<option value="${esc(o.sid)}">${
        esc(o.name||o.sid)}${o.name?' ('+esc(o.sid)+')':''}</option>`).join('')}</select>
      <input class="ro-note" maxlength="200" placeholder="what you know (optional)">
      <button class="save ro-add">add to roster</button></div>`
      :`<div class="empty">already on every flagged org's roster</div>`}
    <p class="blurb">For members a public roster does not show. They carry the
      org's flag like any member, and stay until you remove them — a roster
      refresh never drops them.</p>
  </section>`;
}

// SENTRY had this player, but an org on the profile came without its focus -
// the service that carries focus was down, or there is no key for it. What IS
// known is shown; this says what is not, and why.
function partialHTML(c){
  if(!c.profile_partial) return '';
  return `<div class="noorg partial-profile">SENTRY profile, incomplete:
    ${esc(c.profile_partial)} ${lookupStatusHTML(c,'try now')}</div>`;
}

// Three different claims that all used to render as nothing at all.
function orgStateHTML(c){
  if(c.orgs.length) return '';
  if(!c.known_profile && c.lookup_failed){
    // Not the same as "not looked up": we asked and the service did not
    // answer. Said plainly, because the two used to look identical and a
    // silent "not looked up" on a confident read looked like a missed audit.
    const f=c.lookup_failed;
    const next=f.gave_up
      ? 'gave up for now, tries again next time this contact is read'
      : 'retrying automatically';
    return `<div class="noorg lookup-failed">RSI lookup failed: ${esc(f.error)}
      — ${next} ${lookupStatusHTML(c,'try now')}</div>`;
  }
  if(!c.known_profile)
    return `<div class="noorg unknown-org">not looked up yet
      ${lookupStatusHTML(c,'look up')}</div>`;
  if(!c.verified)
    return `<div class="noorg no-account">no RSI account by this name —
      likely a misread ${lookupStatusHTML(c,'look up again')}</div>`;
  if(c.redacted)
    return `<div class="noorg hidden-org">orgs hidden by this player</div>`;
  return `<div class="noorg">no org</div>`;
}

// One place that turns a stored key into something renderable. Every flag
// lookup goes through here, including keys that have been retired or that
// this install has never heard of - a report must never render as a blank
// chip or a raw database value.
function flagOf(key){
  const m=(data.stats.flagmap||{})[key];
  if(m) return m;
  return {label:String(key||'unknown').replace(/_/g,' '),
          color:'#6b7a8c', polarity:'negative', retired:true};
}

// Saved reports, newest first. 'pending' is excluded - it has its own amber
// prompt above, because it is a question rather than a record.
function reportsHTML(c){
  const rs=(c.reports||[]).filter(r=>r.category!=='pending');
  if(!rs.length) return '';
  const age=d=>d<1?'today':(d<365?d+'d ago':(d/365).toFixed(1)+'y ago');
  return `<div class="rpts">
    <h4>${rs.length} report${rs.length===1?'':'s'}</h4>
    ${rs.map(r=>`
      <div class="rpt" data-id="${r.id}" data-handle="${esc(c.handle)}">
        <div class="rpt-head">
          <span class="rpt-cat" data-cat="${esc(r.category)}"
                style="color:${esc(flagOf(r.category).color)}"
                >${esc(flagOf(r.category).label)}${
                  flagOf(r.category).retired?' <i class="retired">(retired)</i>':''}</span>
          <span class="rpt-when">${age(r.age_days)}${fromHTML(r)}</span>
          <button class="rpt-edit">edit</button>
        </div>
        <div class="rpt-note ${r.note?'':'empty'}">${esc(r.note||'no reason recorded')}</div>
      </div>`).join('')}
  </div>`;
}

// A report someone shared with you: same colours as your own, and a small
// note saying whose it is.
function fromHTML(r){
  return r.source&&r.source!=='self'
    ? ` <span class="rpt-from" title="imported from ${esc(r.source)}'s export">from ${esc(r.source)}</span>`:'';
}

function tile(c){
  const el=document.createElement('div');
  el.className='tile '+c.state;
  el.dataset.handle=c.handle;
  const badges=[];
  if(c.state==='known') badges.push(['bad','known '+(c.score>0?'+'+c.score:'')]);
  c.flags.forEach(f=>badges.push([/PIRACY/.test(f)?'bad':'warn',f]));
  if(c.redacted) badges.push(['','orgs redacted']);
  if(!c.known_profile) badges.push(c.lookup_failed
      ? ['warn','lookup failed'] : ['','not looked up']);
  else if(c.profile_stale) badges.push(['warn',
      'profile '+(c.profile_age_days!=null?c.profile_age_days+'d':'')+' old']);
  else if(!c.verified) badges.push(['bad','no RSI account']);
  if(c.profile_partial) badges.push(['warn','focus unknown']);
  // Only for a confirmed account. A handle RSI says does not exist has no
  // flags because there is nobody to have them, and a green all-clear on
  // the likeliest misread says the opposite of what the tile means.
  if(c.state==='clean'&&!c.flags.length&&c.verified) badges.push(['ok','no flags']);

  el.innerHTML=`
    <div class="row1">
      <span class="handle">${handleHTML(c.handle, c.verified)}</span>
      <span class="range">${c.range_km!=null?c.range_km.toFixed(1)+' km':''}</span>
    </div>
    <div class="meta">
      <span>seen ${c.sightings}×</span>
      <span>${ago(c.last_seen)}</span>
      ${c.enlisted_days!=null?`<span>enlisted ${
        c.enlisted_days<365?c.enlisted_days+'d':Math.floor(c.enlisted_days/365)+'y'}</span>`:''}
    </div>
    ${badges.length?`<div class="badges">${badges.map(([k,t])=>
      `<span class="badge ${k}">${esc(t)}</span>`).join('')}</div>`:''}
    ${c.orgs.length?`<div class="orgs">${orgsHTML(c.orgs,true)}</div>`:''}
    ${orgStateHTML(c)}
    ${partialHTML(c)}
    ${staleHTML(c)}
    ${reportsHTML(c)}
    ${c.reasons.length?`<div class="reasons">${c.reasons.map(r=>
      `<div>▸ ${esc(r)}</div>`).join('')}</div>`:''}
    ${watchHTML(c)}
    <div class="links">
      <a href="${esc(c.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
      <a href="${esc(c.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
    </div>
    ${c.pending?`<div class="pending-note">
      <div class="why">flagged ${c.pending}x — no reason recorded yet</div>
      <div class="pending-row">
        <select class="reason-cat" data-handle="${esc(c.handle)}">
          ${(data.stats.categories||[]).map(f=>
            `<option value="${esc(f.key)}">${esc(f.label)}</option>`).join('')}
        </select>
        <input class="reason-note" data-handle="${esc(c.handle)}"
               placeholder="what happened (optional)" maxlength="500">
        <button class="reason-save" data-handle="${esc(c.handle)}">save</button>
      </div>
    </div>`:''}
    <div class="tile-actions">
      <button class="flagbtn ${c.pending?'done':''}"
              data-handle="${esc(c.handle)}">${c.pending?'flag again':'flag'}</button>
      ${(c.audits||[]).length?`<button class="auditbtn"
         data-handle="${esc(c.handle)}">audit ${c.audits.length}</button>`:''}
      <button class="forget ${c.handle===armedHandle?'armed':''}"
              data-handle="${esc(c.handle)}">${c.handle===armedHandle
                ?'delete '+esc(c.handle)+'? click again':'forget'}</button>
    </div>`;
  return el;
}

// Deleting is two clicks, not one: 'forget' arms, a second click inside six
// seconds commits. The poll rebuilds the whole grid every 2s, which would rip
// an armed button out from under the second click, so rendering pauses while
// something is armed - and disarms itself so a stray arm cannot freeze the UI.
// How long an armed confirm stays live. The first version used SIX seconds,
// which was far too short: a human reads "delete WASTE? click again", thinks
// about it, and clicks after eight - by then it had silently disarmed, so the
// second click just re-armed it and nothing ever happened. Reported from a
// live session as "the second click didn't work", and it loops forever.
//
// It only had a fuse because arming used to SUPPRESS rendering, and a stray
// arm would have frozen the grid. Armed state is now rendered FROM state, so
// it survives a rebuild and the timeout is just tidiness.
const ARM_MS=20000;
let armedHandle=null, armedTimer=null;

function disarm(){
  armedHandle=null;
  if(armedTimer){clearTimeout(armedTimer); armedTimer=null;}
  render();
}

// Editing a saved report, in place on the tile. A report gets written before
// the story is complete - a flag pressed mid-fight, a category guessed at, a
// note added later when the same person does it twice - so amending one is
// finishing the record rather than falsifying it. The timestamp is never
// touched: a report dates the incident, not the paperwork.
grid.addEventListener('click', async ev=>{
  const editBtn=ev.target.closest('.rpt-edit');
  if(editBtn){
    const box=editBtn.closest('.rpt');
    if(box.querySelector('.rpt-edit-row')) return;   // already open
    // dataset.cat, NOT the rendered text. Recovering a key by reversing the
    // display string worked only while labels were the keys with underscores
    // swapped for spaces; "Camping a location" would have become
    // Camping_a_location and the edit would have failed validation.
    const cat=box.querySelector('.rpt-cat').dataset.cat;
    const noteEl=box.querySelector('.rpt-note');
    const note=noteEl.classList.contains('empty')?'':noteEl.textContent.trim();
    const cats=data.stats.categories||[];
    const row=document.createElement('div');
    row.className='rpt-edit-row';
    row.innerHTML=`
      <select class="rpt-cat-in">${cats.map(f=>
        `<option value="${esc(f.key)}" ${f.key===cat?'selected':''}>${esc(f.label)}</option>`
      ).join('')}${cats.some(f=>f.key===cat)?'':
        `<option value="${esc(cat)}" selected>${esc(flagOf(cat).label)} (retired)</option>`
      }</select>
      <input class="rpt-note-in" maxlength="500" placeholder="what happened"
             value="${esc(note)}">
      <button class="rpt-save">save</button>
      <button class="cancel rpt-cancel">cancel</button>`;
    box.appendChild(row);
    row.querySelector('.rpt-note-in').focus();
    return;
  }

  if(ev.target.closest('.rpt-cancel')){
    ev.target.closest('.rpt-edit-row').remove();
    // The render that was skipped while this field had focus is replayed on
    // blur; removing the row without that leaves the tile a frame behind.
    render();
    return;
  }

  const save=ev.target.closest('.rpt-save');
  if(!save) return;
  const box=save.closest('.rpt'), row=save.closest('.rpt-edit-row');
  const id=Number(box.dataset.id);
  const category=row.querySelector('.rpt-cat-in').value;
  const note=row.querySelector('.rpt-note-in').value;
  save.disabled=true; save.textContent='saving…';
  try{
    // Blur first: the save is supposed to trigger a refresh, and the guard
    // would suppress the very render it causes while this input holds focus.
    row.querySelector('.rpt-note-in').blur();
    const j=await post('/api/report/edit',{id, category, note});
    if(!j.ok) throw new Error(j.error||'failed');
    say(`updated ${box.dataset.handle} — ${flagOf(category).label}`);
    await poll(); render();
  }catch(e){
    say('could not save: '+e.message);
    save.disabled=false; save.textContent='save';
  }
});

grid.addEventListener('click', async ev=>{
  const btn=ev.target.closest('.forget');
  if(!btn) return;
  const handle=btn.dataset.handle;
  if(armedHandle!==handle){
    armedHandle=handle;
    if(armedTimer) clearTimeout(armedTimer);
    armedTimer=setTimeout(disarm,ARM_MS);
    render();
    return;
  }
  btn.disabled=true; btn.textContent='deleting...';
  try{
    const r=await fetch('/api/forget',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({handle})});
    const j=await r.json();
    if(!j.ok) throw new Error(j.error||'failed');
    const n=j.removed||{};
    say('forgot '+j.handle+' — '+
      (n.sightings||0)+' sighting(s), '+(n.reports||0)+' report(s)');
    armedHandle=null;
    if(armedTimer){clearTimeout(armedTimer); armedTimer=null;}
    await poll();
  }catch(e){
    btn.disabled=false; btn.textContent='forget';
    say('delete failed: '+e.message);
    disarm();
  }
});

// ---- detail window -------------------------------------------------------
// Opened by clicking anywhere on a tile that is not itself a control. This is
// where anything gets UNDONE or corrected: a flag is one button press, so some
// of them are tests, misfires or the wrong tile, and until there was a way to
// take one back the only undo was forget() - which throws the sighting history
// away too, making a mis-click cost more than the mistake did.
const veil=document.getElementById('veil'), modal=document.getElementById('modal');
let openHandle=null;

function contactByHandle(h){
  return (data.contacts||[]).find(c=>c.handle===h);
}

function closeModal(){ openHandle=null; lookupOpen=false; modalEdit=null;
  veil.hidden=true; render(); }

function renderModal(){
  const c=contactByHandle(openHandle);
  if(!c){ closeModal(); return; }
  const cats=data.stats.categories||[];
  const age=d=>d<1?'today':(d<365?d+'d ago':(d/365).toFixed(1)+'y ago');
  modal.innerHTML=`
    <button class="close" title="close">&times;</button>
    <h2>${esc(c.handle)}</h2>
    <div class="sub">
      ${c.sightings} sighting${c.sightings===1?'':'s'}
      ${enlistedHTML(c)}
      ${c.range_km!=null?` · last seen ${Number(c.range_km).toFixed(1)} km`:''}
      · score ${c.score}
    </div>
    ${modalEdit&&modalEdit.kind==='enlisted'?enlistedEditorHTML(c):''}
    ${(c.flags.length||c.reasons.length)?`<section><h3>flags</h3>${
      c.flags.concat(c.reasons).map(f=>
      `<div>▸ ${esc(f)}</div>`).join('')}</section>`:''}
    ${(c.watchlist||[]).length?`<section>
      <h3>on someone else's watchlist</h3>
      <p class="blurb">Not your judgement and not counted in your score. Check
         the source before you act on it.</p>
      ${watchHTML(c)}</section>`:''}
    ${c.orgs.length?`<section><h3>orgs</h3>${orgsHTML(c.orgs,false,true)}</section>`:''}
    ${rosterSectionHTML(c)}
    ${(c.audits||[]).length?`<section>
      <h3>audit — ${c.audits.length} uncertain read${c.audits.length===1?'':'s'}</h3>
      ${c.audits.map(a=>`
        <div class="aud" data-audit="${a.id}">
          <div class="meta">read as <b>${esc(a.read_as||a.handle)}</b>${
            (a.read_as&&a.read_as!==a.handle)
              ? ` <span style="color:var(--muted)">(stored as ${esc(a.handle)})</span>`:''} at
            ${a.range_km==null?'?':Number(a.range_km).toFixed(1)} km ·
            ${a.votes}/${a.total} votes ·
            ${new Date(a.ts*1000).toLocaleString()}</div>
          ${a.reason==='no_rsi_account'?`<div class="warn">RSI has no account
             by this name — the read is probably wrong even though the frames
             agreed on it</div>`:''}
          ${a.files_missing?`<div class="warn">the images for this audit are
             gone — nothing left to check, dismiss it</div>`:''}
          ${a.crop_name?`<img class="crop" src="/${esc(a.crop_name)}" alt="name crop">`:''}
          ${a.crop_range?`<img class="crop" src="/${esc(a.crop_range)}" alt="range crop">`:''}
          <div class="cands">
            <button data-pick="${esc(a.read_as||a.handle)}">${esc(a.read_as||a.handle)}</button>
            ${(a.read_as&&a.read_as!==a.handle)
              ? `<button data-pick="${esc(a.handle)}">${esc(a.handle)}</button>`:''}
            ${(a.variants||[]).map(v=>
              `<button data-pick="${esc(v[0])}">${esc(v[0])} ×${v[1]}</button>`).join('')}
          </div>
          <div class="manual">
            <!-- 40 to match HANDLE_RE on the server. It was 24, which is not
                 a rule anything else enforced: a longer handle was silently
                 TRUNCATED to 24 characters, and the truncation still passed
                 validation, so the audit recorded a name nobody had typed.
                 Measured against 117 real RSI handles - the longest of
                 them is 25 characters. -->
            <input class="aud-name" placeholder="or type what it actually says"
                   maxlength="40" value="">
            <button class="save aud-confirm">confirm</button>
            <button class="del aud-dismiss">dismiss</button>
            ${a.frame_path?`<a class="links" href="/${esc(a.frame_path)}"
               target="_blank" rel="noopener"
               style="border:1px solid var(--line);padding:4px 9px;color:var(--muted);
                      text-decoration:none;font-size:10px;letter-spacing:.1em;
                      text-transform:uppercase">full frame</a>`:''}
          </div>
        </div>`).join('')}
    </section>`:''}
    <section>
      <h3>reports${c.reports.length?` — ${c.reports.length}`:''}</h3>
      ${c.reports.length?c.reports.map(r=>`
        <div class="rep ${r.category==='pending'?'ispending':''}" data-id="${r.id}">
          <span class="when">${age(r.age_days)}${fromHTML(r)}</span>
          <select class="r-cat">
            ${r.category==='pending'?'<option value="pending">awaiting reason</option>':''}
            ${cats.map(f=>`<option value="${esc(f.key)}"${f.key===r.category?' selected':''}>
              ${esc(f.label)}</option>`).join('')}
            ${(r.category==='pending'||cats.some(f=>f.key===r.category))?'':
              `<option value="${esc(r.category)}" selected>${
                esc(flagOf(r.category).label)} (retired)</option>`}
          </select>
          <input class="r-note" value="${esc(r.note||'')}"
                 placeholder="what happened" maxlength="500">
          <button class="save" data-id="${r.id}">save</button>
          <button class="del ${String(r.id)===armedDel?'armed':''}"
                  data-id="${r.id}">${String(r.id)===armedDel
                    ?'remove? click again':'remove'}</button>
        </div>`).join(''):'<div class="empty">no reports yet</div>'}
    </section>
    <section>
      <h3>add a report</h3>
      <div class="rep">
        <select class="add-cat">${cats.map(f=>
          `<option value="${esc(f.key)}">${esc(f.label)}</option>`).join('')}</select>
        <input class="add-note" placeholder="what happened" maxlength="500">
        <button class="save add-go">add</button>
      </div>
    </section>
    <section>
      <div class="rep">
        <span class="links">
          <a href="${esc(c.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
          <a href="${esc(c.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
        </span>
        <span style="flex:1"></span>
        <button class="del forget-all ${armedDel==='forget'?'armed':''}"
                data-handle="${esc(c.handle)}">${armedDel==='forget'
                  ?'delete everything? click again':'forget this contact'}</button>
      </div>
    </section>
    <section>
      <h3>not a person?</h3>
      <div class="notperson">
        <p>Some things the HUD draws look exactly like a contact — a floating
           salvage crate gets a label and a range line like anybody else.
           Ignoring one deletes its record and stops it being reported again,
           including OCR variants of the same string.</p>
        <div class="np-row">
          <input class="np-note" maxlength="200"
                 placeholder="what it actually is (optional)">
          <button class="del np-go ${armedDel==='ignore'?'armed':''}"
                  data-handle="${esc(c.handle)}">${armedDel==='ignore'
                    ?'ignore '+esc(c.handle)+' forever? click again'
                    :'not a player — always ignore'}</button>
        </div>
      </div>
    </section>`;
  veil.hidden=false;
}

// ---- manual lookup ----------------------------------------------------
// Check a handle you were told rather than one you saw. Looking up and
// storing are deliberately two clicks: most lookups are "who is this?" and
// should leave nothing behind, because a handle typed to check it is not a
// sighting and storing one would put somebody in the database you never met.
let lookupState={handle:'', result:null, busy:false, error:''};

function renderLookup(){
  const r=lookupState.result;
  const age=d=>d==null?'':(d<365?d+'d':(d/365).toFixed(1)+'y');
  let body='';
  if(lookupState.busy){
    body=`<div class="lk-note">looking up…</div>`;
  }else if(lookupState.error){
    body=`<div class="lk-note bad">${esc(lookupState.error)}</div>`;
  }else if(r && !r.exists){
    body=`<div class="lk-note bad">no RSI account called ${esc(r.handle)}${r.fresh
      ?' — asked RSI just now, not the cache':''}.
      If you read this off the HUD it is almost certainly a misread.</div>`;
  }else if(r){
    body=`
      <div class="lk-card">
        <div class="lk-top">
          <span class="handle">${handleHTML(r.handle, r.exists)}</span>
          ${r.moniker&&r.moniker.toUpperCase()!==r.handle
            ? `<span class="lk-mon">${esc(r.moniker)}</span>`:''}
          ${r.enlisted_days!=null
            ? `<span class="lk-en">enlisted ${age(r.enlisted_days)}</span>`:''}
        </div>
        ${r.fresh||r.refreshed||r.source?`<div class="lk-note">${
          r.source==='sentry'
            ? `answered by SENTRY, from its copy of RSI${r.observed_days
                ? ` read ${age(r.observed_days)} ago`:' read today'}`
            : (r.fresh?'asked RSI just now, not the cache':'')}${
          (r.fresh||r.source)&&r.refreshed?' — ':''}${r.refreshed
          ?'your stored record for this contact was updated':''}</div>`:''}
        ${r.stale?`<div class="lk-note bad">incomplete: ${esc(r.stale)}</div>`:''}
        ${r.flagged_orgs.length?`<div class="lk-note bad">member of flagged org
          ${r.flagged_orgs.map(esc).join(', ')}</div>`:''}
        ${r.reasons.length?`<div class="reasons">${r.reasons.map(x=>
          `<div>▸ ${esc(x)}</div>`).join('')}</div>`:''}
        ${r.orgs.length?`<div class="orgs">${orgsHTML(r.orgs,false)}</div>`
          : (r.redacted
              ? `<div class="noorg hidden-org">orgs hidden by this player</div>`
              : `<div class="noorg">no org</div>`)}
        ${r.orgs.length&&r.redacted
          ? `<div class="noorg hidden-org">plus orgs this player hides</div>`:''}
        <div class="links">
          <a href="${esc(r.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
          <a href="${esc(r.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
        </div>
        <div class="lk-actions">
          ${r.stored
            ? `<span class="lk-note">already in your database</span>
               <button class="lk-open" data-handle="${esc(r.handle)}">open record</button>`
            : `<button class="save lk-add" data-handle="${esc(r.handle)}">add to database</button>`}
        </div>
      </div>`;
  }
  modal.innerHTML=`
    <button class="close" title="close">&times;</button>
    <h2>look up a player</h2>
    <div class="sub">Asks RSI directly. Nobody new is stored unless you say so;
      a contact you already have gets their record refreshed.</div>
    <div class="lk-row">
      <input id="lk-in" placeholder="handle, exactly as spelled" maxlength="40"
             value="${esc(lookupState.handle)}" autocomplete="off">
      <button class="save" id="lk-go" ${lookupState.busy?'disabled':''}>look up</button>
    </div>
    ${body}`;
  veil.hidden=false;
  const inp=document.getElementById('lk-in');
  if(inp && !lookupState.busy){ inp.focus(); inp.select(); }
}

function openLookup(){ openHandle=null; lookupOpen=true; renderLookup(); }

// From a tile's look-up button: the same window, already filled in, already
// running - and FRESH, because pressing it means the cached answer (or the
// lack of one) is the thing in doubt.
function lookupContact(handle){
  openHandle=null; lookupOpen=true;
  lookupState={handle, result:null, busy:false, error:''};
  renderLookup();
  doLookup(false, true);
}

async function doLookup(add, fresh){
  const inp=document.getElementById('lk-in');
  const handle=(inp?inp.value:lookupState.handle).trim();
  if(!handle) return;
  lookupState={handle, result:lookupState.result, busy:true, error:''};
  renderLookup();
  try{
    const j=await post('/api/lookup',{handle, add:!!add, fresh:!!fresh});
    if(!j.ok){ lookupState={handle, result:null, busy:false, error:j.error}; }
    else{ lookupState={handle, result:j, busy:false, error:''};
          if(j.message) say(j.message); }
    if(add) await poll();
  }catch(e){
    lookupState={handle, result:null, busy:false, error:e.message};
  }
  renderLookup();
}

modal.addEventListener('click', async ev=>{
  if(!lookupOpen) return;
  if(ev.target.id==='lk-go'){ await doLookup(false); return; }
  if(ev.target.closest('.lk-add')){ await doLookup(true); return; }
  const open=ev.target.closest('.lk-open');
  if(open){ lookupOpen=false; openModal(open.dataset.handle); }
});
modal.addEventListener('keydown', ev=>{
  if(lookupOpen && ev.key==='Enter' && ev.target.id==='lk-in') doLookup(false);
});

function openModal(handle){ openHandle=handle; modalEdit=null; renderModal(); }

grid.addEventListener('click', ev=>{
  const aud=ev.target.closest('.auditbtn');
  if(aud){ openModal(aud.dataset.handle); return; }
  const lk=ev.target.closest('.lookupbtn');
  if(lk){ startTileLookup(lk.dataset.handle); return; }
  if(ev.target.closest('button,a,select,input')) return;
  const tile=ev.target.closest('.tile');
  if(tile && tile.dataset.handle) openModal(tile.dataset.handle);
});
veil.addEventListener('click', ev=>{ if(ev.target===veil) closeModal(); });
document.addEventListener('keydown', ev=>{ if(ev.key==='Escape'&&openHandle) closeModal(); });

let stickyUntil=0;
function say(msg){ updated.textContent=msg; stickyUntil=Date.now()+6000; }

// A lookup runs on the server's own thread, so its result arrives with a later
// stream frame rather than in the POST response. Watch for it and report -
// otherwise a handle that does not resolve looks exactly like one that does,
// which is the case most worth knowing about.
function watchLookup(handle){
  const h=(handle||'').toUpperCase();
  let tries=0;
  const t=setInterval(()=>{
    const msg=(data.stats&&data.stats.lookups||{})[h];
    if(msg && !/looking up/.test(msg)){ clearInterval(t); say(msg); }
    else if(++tries>20){ clearInterval(t); }
  },700);
}

async function post(url, payload){
  const r=await fetch(url,{method:'POST',
    headers:{'Content-Type':'application/json'},
    body:JSON.stringify(payload)});
  const j=await r.json();
  // needs_confirm is a QUESTION, not a failure. confirm_audit refuses to merge
  // two spellings that both resolve to real accounts and hands back ok:false
  // with needs_confirm:true so the UI can ask. Throwing on it meant the
  // "merge anyway" branch below could never run: correcting a name to another
  // real handle just said "failed" and left you no way through - which looked
  // exactly like the audit box rejecting a name it did not expect.
  if(!j.ok && j.needs_confirm!==true) throw new Error(j.error||'failed');
  return j;
}

grid.addEventListener('click', async ev=>{
  const flag=ev.target.closest('.flagbtn');
  if(flag){
    const handle=flag.dataset.handle;
    flag.disabled=true;
    try{
      await post('/api/flag',{handle});
      say('flagged '+handle+' — add the reason when you land');
      await poll();
    }catch(e){ say('flag failed: '+e.message); flag.disabled=false; }
    return;
  }
  const save=ev.target.closest('.reason-save');
  if(save){
    const handle=save.dataset.handle;
    const cat=grid.querySelector(`.reason-cat[data-handle="${CSS.escape(handle)}"]`);
    const note=grid.querySelector(`.reason-note[data-handle="${CSS.escape(handle)}"]`);
    save.disabled=true;
    try{
      const j=await post('/api/reason',
        {handle, category:cat.value, note:note.value});
      say('recorded '+j.resolved+' report(s) for '+handle+
        ' as '+cat.value);
      // Drop focus first, or editingInGrid() is still true and the refresh
      // this poll is supposed to do gets skipped.
      note.blur(); cat.blur();
      await poll();
    }catch(e){ say('save failed: '+e.message); save.disabled=false; }
  }
});

// True whenever the cursor is in a control inside a tile.
//
// The poll rebuilds the whole grid every 2s, and replaceChildren destroys
// whatever had focus. The first version of this guard only paused for a
// .reason-note that ALREADY had text, which got it exactly backwards: an empty
// field is the one you are about to type into, so it lost focus after at most
// two seconds and you could never get a first character in. An open <select>
// was not covered at all, so the dropdown shut itself.
//
// Any focused input or select inside the grid pauses rendering. Click away and
// it resumes on the next poll.
function editingInGrid(){
  const a = document.activeElement;
  if(!a || !grid.contains(a)) return false;
  return a.tagName==='INPUT' || a.tagName==='SELECT' || a.tagName==='TEXTAREA';
}

// The modal has the same problem the grid does - a poll every 2s must not wipe
// a half-edited note or a chosen category out from under you.
function editingInModal(){
  const x=document.activeElement;
  if(!x || veil.hidden || !modal.contains(x)) return false;
  return x.tagName==='INPUT'||x.tagName==='SELECT'||x.tagName==='TEXTAREA';
}

let armedDel=null;

function openEditor(){
  renderModal();
  const f=modal.querySelector('.medit .me-f');
  if(f) f.focus();
}
modal.addEventListener('input', ev=>{
  const f=ev.target.closest('.me-f');
  if(f && modalEdit) modalEdit.vals[f.dataset.k]=f.value;
});
modal.addEventListener('change', ev=>{
  const f=ev.target.closest('.me-f');
  if(f && modalEdit) modalEdit.vals[f.dataset.k]=f.value;
});
modal.addEventListener('keydown', ev=>{
  if(ev.key!=='Enter') return;
  if(ev.target.closest('.medit .me-f')) modal.querySelector('.me-save')?.click();
  else if(ev.target.classList.contains('ro-note')) modal.querySelector('.ro-add')?.click();
});

modal.addEventListener('click', async ev=>{
  const t=ev.target;
  if(t.closest('.close')){ closeModal(); return; }

  // ---- your own entries ----
  const oe=t.closest('[data-orgedit]');
  if(oe){ modalEdit={kind:'org', sid:oe.dataset.orgedit, vals:{}}; openEditor(); return; }
  if(t.closest('[data-edit="enlisted"]')){ modalEdit={kind:'enlisted', vals:{}}; openEditor(); return; }
  if(t.closest('.me-cancel')){ modalEdit=null; renderModal(); return; }
  const ms=t.closest('.me-save'), mc=t.closest('.me-clear');
  if((ms||mc) && modalEdit){
    const ed=modalEdit;
    modal.querySelectorAll('.medit .me-f').forEach(f=>{ ed.vals[f.dataset.k]=f.value; });
    const v=mc?{}:ed.vals;
    if(ms && !(v.primary||v.secondary||v.name||v.enlisted)){
      say(ed.kind==='org'?'pick a focus first':'pick a date first'); return; }
    try{
      if(ed.kind==='org'){
        const j=await post('/api/org/manual',{sid:ed.sid, primary:v.primary||'',
          secondary:v.secondary||'', name:v.name||''});
        say(j.message);
      }else{
        const j=await post('/api/player/enlisted',{handle:openHandle, enlisted:v.enlisted||''});
        say(j.enlisted?`saved your date for ${openHandle}`:`cleared your date for ${openHandle}`);
      }
      modalEdit=null; await poll(); renderModal();
    }catch(e){ say('could not save: '+e.message); }
    return;
  }
  const ra=t.closest('.ro-add');
  if(ra){
    const sid=modal.querySelector('.ro-sid').value;
    const noteEl=modal.querySelector('.ro-note');
    ra.disabled=true;
    try{
      const j=await post('/api/org/member/add',{sid, handle:openHandle, note:noteEl.value});
      say(j.message); noteEl.blur(); await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); ra.disabled=false; }
    return;
  }
  const rr=t.closest('.ro-rm');
  if(rr){
    const key='ro:'+rr.dataset.rosid;
    if(armedDel!==key){
      armedDel=key;
      setTimeout(()=>{ if(armedDel===key){ armedDel=null; renderModal(); } },ARM_MS);
      renderModal(); return;
    }
    armedDel=null;
    try{
      const j=await post('/api/org/member/remove',{sid:rr.dataset.rosid, handle:openHandle});
      say(j.message); await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); }
    return;
  }

  const np=t.closest('.np-go');
  if(np){
    if(armedDel!=='ignore'){
      armedDel='ignore';
      if(armedTimer) clearTimeout(armedTimer);
      armedTimer=setTimeout(()=>{armedDel=null;renderModal();},ARM_MS);
      renderModal();
      return;
    }
    const note=(modal.querySelector('.np-note')||{}).value||'';
    np.disabled=true; np.textContent='ignoring…';
    try{
      const j=await post('/api/ignore',{handle:np.dataset.handle, note});
      if(!j.ok) throw new Error(j.error||'failed');
      armedDel=null;
      const gone=j.removed||{};
      say(`${j.label} will not be reported again`+
          (gone.sightings?` — removed ${gone.sightings} sighting(s)`:''));
      closeModal();
      await poll(); render();
    }catch(e){ say('could not ignore: '+e.message);
               np.disabled=false; renderModal(); }
    return;
  }

  // Only the buttons this owns: a report's remove, and forget. .del is a
  // COLOUR, and the audit's dismiss and "merge anyway" wear it too - this
  // used to catch them first, so dismiss never dismissed (one click armed a
  // report removal with no id, the second posted it and said "failed"), and
  // merge anyway re-rendered the window out from under its own answer.
  const del=t.closest('.del[data-id], .del.forget-all');
  if(del){
    const isForget=del.classList.contains('forget-all');
    const key=isForget?'forget':del.dataset.id;
    if(armedDel!==key){
      armedDel=key;
      setTimeout(()=>{ if(armedDel===key){ armedDel=null; renderModal(); } },ARM_MS);
      renderModal();
      return;
    }
    armedDel=null; del.disabled=true;
    try{
      if(isForget){
        await post('/api/forget',{handle:openHandle});
        say('forgot '+openHandle);
        closeModal(); await poll();
      }else{
        const j=await post('/api/report/delete',{id:Number(del.dataset.id)});
        const g=j.deleted;
        say(g?('removed '+g.category+' report on '+j.handle)
              :'report already gone');
        await poll(); renderModal();
      }
    }catch(e){ say('failed: '+e.message); del.disabled=false; }
    return;
  }

  // ---- audit ----
  const pick=t.closest('[data-pick]');
  if(pick){
    pick.closest('.aud').querySelector('.aud-name').value=pick.dataset.pick;
    return;
  }
  const dismiss=t.closest('.aud-dismiss');
  if(dismiss){
    const box=dismiss.closest('.aud');
    dismiss.disabled=true;
    try{
      await post('/api/audit/dismiss',{id:Number(box.dataset.audit)});
      say('audit dismissed — reading left as it was');
      await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); dismiss.disabled=false; }
    return;
  }
  const conf=t.closest('.aud-confirm');
  if(conf){
    const box=conf.closest('.aud');
    const name=box.querySelector('.aud-name').value.trim();
    if(!name){ say('pick a candidate or type the name first'); return; }
    conf.disabled=true;
    try{
      let j=await post('/api/audit/confirm',
        {id:Number(box.dataset.audit), name});
      if(j.needs_confirm===true){
        // Both spellings are real accounts. This is the failure the whole
        // identity rule exists to prevent, so it is asked out loud rather
        // than resolved quietly.
        const w=document.createElement('div');
        w.className='warn';
        w.textContent=j.error+'. Merge anyway?';
        const yes=document.createElement('button');
        yes.className='del'; yes.textContent='merge anyway';
        yes.onclick=async()=>{
          try{
            await post('/api/audit/confirm',
              {id:Number(box.dataset.audit), name, force:true});
            say('merged '+j.old+' into '+j.new+' — looking up on RSI…');
            watchLookup(j.new);
            openHandle=j.new;
            await poll(); renderModal();
          }catch(e){ say('failed: '+e.message); }
        };
        w.appendChild(document.createTextNode(' '));
        w.appendChild(yes);
        box.appendChild(w);
        conf.disabled=false;
        return;
      }
      // A rename with checked===false means no profile backend was available
      // to ask whether the two spellings are two different people. The merge
      // is still what was asked for, but it went through unverified and that
      // should not be silent.
      const blind = j.renamed && j.checked === false;
      // Three outcomes, and they are not the same thing. adopted means the
      // record this audit pointed at was already gone, so the correction was
      // kept as a new record rather than thrown away - say so, because
      // "corrected X to Y" would imply history moved across and none did.
      const what = j.adopted
        ? ('recorded '+j.new+' — the old record for '+j.old+' was already gone')
        : j.renamed ? ('corrected '+j.old+' to '+j.new)
                    : ('confirmed '+j.new);
      say(what
          + (blind?' — no identity check, add an API key to enable one':'')
          + (j.lookup?' — looking up on RSI…':''));
      if(j.lookup) watchLookup(j.new);
      // Follow the rename. openHandle still points at the old spelling, which
      // no longer exists, so the window would sit on a dead record.
      if(j.renamed || j.adopted) openHandle=j.new;
      await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); conf.disabled=false; }
    return;
  }

  const save=t.closest('.save');
  if(!save) return;
  try{
    if(save.classList.contains('add-go')){
      const cat=modal.querySelector('.add-cat').value;
      const note=modal.querySelector('.add-note').value;
      await post('/api/report/add',{handle:openHandle,category:cat,note});
      say('added '+cat+' report on '+openHandle);
    }else{
      const row=save.closest('.rep');
      await post('/api/report/edit',{id:Number(save.dataset.id),
        category:row.querySelector('.r-cat').value,
        note:row.querySelector('.r-note').value});
      say('updated report on '+openHandle);
    }
    document.activeElement.blur();
    await poll(); renderModal();
  }catch(e){ say('failed: '+e.message); }
});

// ---- settings ---------------------------------------------------------
// Rendered from data.stats.settings_schema, so this function never mentions a
// specific setting. Adding one is a Field in settings.py.
//
// Deliberately NOT re-rendered on every stream frame: the panel is full of
// inputs, and rebuilding it under the cursor is exactly the race that kept
// wiping half-typed notes in the grid. It is drawn when opened, when you
// switch category, and after a save replies - never on a timer.

const JOBS=[
  {id:'ghosts', title:'Remove handles with no RSI account',
   body:'Looks every stored handle up and deletes the ones the API says do '+
        'not exist - OCR inventions like ONNCARSTE for VONCARSTEIN. A handle '+
        'with a report against it, or one waiting on an audit, is never '+
        'touched: that is evidence, and an audit is how a misread gets '+
        'corrected rather than deleted. Rate limited to one lookup a second, '+
        'so a large database takes a minute.',
   verb:'delete them'},
  {id:'backfill', title:'Look up contacts that were never checked',
   body:'Finds contacts seen often enough to meet the evidence bar but with '+
        'no RSI profile stored, or one still missing an org focus, and '+
        'fetches them. The watcher normally does '+
        'this as it goes; one can still be missed if a lookup failed or the '+
        'watcher stopped mid-burst. One request a second.',
   verb:'look them up'},
  {id:'restart', title:'Restart the watcher',
   body:'Most capture and detection settings are read once when the watcher '+
        'starts, so changing one does nothing until it restarts. This asks it '+
        'to reload itself in place - it picks the request up while idle, '+
        'within a second or two. Nothing happens if no watcher is running.',
   verb:'restart it', direct:true},
  {id:'frames', title:'Delete old capture frames',
   body:'Applies the retention rules above to debug_bursts/, live_frames/ '+
        'and probe_frames/ right now, and prunes audits past their age limit '+
        'along with any image no audit row points at.',
   verb:'delete them'},
];

function setVal(k){ return (data.stats.settings||{})[k]; }

// Settings that take an input binding rather than a plain string.
// Which settings get a "detect" button, DERIVED from the key name rather than
// listed. The list version was ['key','focus_key','chat_key'], and adding
// dual_key to the schema silently produced a binding field with no way to
// detect a binding - the one control that genuinely needs one, because four
// identical joysticks cannot be told apart any other way.
//
// Every binding setting is named `key` or `something_key`, and nothing else
// is, so the convention is the rule.
const isBinding=k=>k==='key'||k.endsWith('_key');

function fieldHTML(f,v){
  const id='set_'+f.key;
  let ctl;
  if(f.type==='bool'){
    ctl=`<input type="checkbox" id="${id}" data-key="${f.key}" ${v?'checked':''}>`;
  }else if(f.type==='choice'){
    ctl=`<select id="${id}" data-key="${f.key}">`+
      f.choices.map(c=>`<option value="${esc(c)}" ${c===v?'selected':''}>${esc(c)}</option>`)
      .join('')+`</select>`;
  }else if(f.type==='int'||f.type==='float'){
    const step=f.type==='int'?'1':'any';
    ctl=`<input type="number" id="${id}" data-key="${f.key}" value="${esc(v)}"`+
        ` step="${step}"${f.lo!=null?` min="${f.lo}"`:''}${f.hi!=null?` max="${f.hi}"`:''}>`;
  }else if(isBinding(f.key)){
    // Four joysticks here all report "HID-compliant game controller", so a
    // dropdown of device names would be four identical rows. Pressing the
    // thing you want is the only way to tell them apart.
    ctl=`<input type="text" id="${id}" data-key="${f.key}" value="${esc(v)}">`+
        `<button class="detect" data-detect="${f.key}">detect</button>`;
  }else{
    ctl=`<input type="text" id="${id}" data-key="${f.key}" value="${esc(v)}">`;
  }
  const changed = JSON.stringify(v)!==JSON.stringify(f.default);
  const shown = d=>f.type==='bool' ? (d?'on':'off') : String(d);
  return `<div class="setrow${changed?' changed':''}${f.advanced?' adv':''}">
    <div>
      <label class="lab" for="${id}">${esc(f.label)}</label>
      ${f.help?`<div class="hint">${esc(f.help)}</div>`:''}
      ${f.restart?`<div class="needs">needs a ${esc(f.restart)} restart</div>`:''}
      ${changed?`<button class="reset" data-reset="${f.key}"
         title="back to ${esc(shown(f.default))}">reset · default ${
         esc(shown(f.default))}</button>`:''}
    </div>
    <div class="ctl">${ctl}<span class="unit">${esc(f.unit||'')}</span></div>
  </div>`;
}

// The reasons you can file a report under.
//
// Each row edits a LABEL, a colour and a POLARITY. The key behind it is
// hidden and permanent, which is what makes renaming safe: every report ever
// filed keeps pointing at the same key, so changing "Camping" to "Spawn
// camping" renames three years of history rather than orphaning it.
//
// Clearing a label retires that flag instead of deleting it - it leaves this
// list and the dropdowns, and the reports filed under it keep their name and
// colour. Retired rows are shown greyed at the bottom so they can be revived.
function flagsHTML(){
  const all=Object.entries(data.stats.flagmap||{})
    .map(([k,v])=>Object.assign({key:k}, v));
  const live=all.filter(f=>!f.retired);
  const dead=all.filter(f=>f.retired);
  const free=Math.max(0, (data.stats.flag_slots||12)-live.length);
  const row=(f,isNew)=>`
    <div class="flagrow${f.retired?' flag-dead':''}" data-key="${esc(f.key||'')}">
      <input class="fl-color" type="color" value="${esc(f.color||'#8fa6bb')}"
             title="colour for this flag">
      <input class="fl-label" maxlength="32" placeholder="${
        isNew?'a new reason…':'name (clear it to retire)'}"
             value="${esc(f.label||'')}">
      <select class="fl-pol" title="how this flag affects the score">
        <option value="negative"${f.polarity==='negative'?' selected':''}>counts against</option>
        <option value="positive"${f.polarity==='positive'?' selected':''}>counts for</option>
        <option value="neutral"${f.polarity==='neutral'?' selected':''}>does not score</option>
      </select>
      <input class="fl-say" maxlength="48" placeholder="spoken as (optional)"
             value="${esc(f.say||'')}" title="what speech should say instead of the name">
    </div>`;
  return `<div class="job"><h3>Report reasons</h3>
    <p>The list you pick from when you flag somebody. Rename these freely —
       the reports you have already filed follow the rename, because each row
       has a hidden permanent id and only its <em>name</em> is what you see.</p>
    <p class="blurb"><b>Counts against / counts for / does not score</b> is not
       decoration. The score decides whether a contact alerts at all. A reason
       set to "counts against" adds to it, "counts for" subtracts — so a
       reason like <i>Friendly</i> or <i>Good trader</i> must be set to counts
       for, or filing it would make that person look <em>more</em> dangerous.
       "Does not score" is for observations you want recorded but not judged.
       Reports fade to half weight after 180 days either way.</p>
    <p class="blurb">Clearing a name <b>retires</b> that reason rather than
       deleting it: it disappears from the dropdowns, and every report already
       filed under it keeps its name and colour forever. Type a name back in
       to bring it out of retirement. "Spoken as" is what the voice says for
       this reason (the {label} in Spoken alerts) — a name like "KOS!!!"
       reads fine and speaks badly. Blank uses the name.</p>
    <div class="flags">
      ${live.map(f=>row(f,false)).join('')}
      ${Array.from({length:free},()=>row({},true)).join('')}
    </div>
    ${dead.length?`<div class="blurb" style="margin-top:10px">retired —
       still used by existing reports, type a name to revive:</div>
      <div class="flags">${dead.map(f=>row(f,false)).join('')}</div>`:''}
    <div class="bundlerow">
      <button class="openfolder" id="flags-save">save reasons</button>
    </div>
    <div class="out" id="flags-out"></div>
    </div>`;
}

// Where everything actually is, with a button that opens it.
//
// The full path is printed next to each button and is selectable, because the
// button only works when the browser is on the same machine as the server -
// and the path is what you need when it is not. Two answers to one question,
// and the text one always works.
function foldersHTML(){
  const f=data.stats.folders||[];
  if(!f.length) return '';
  return `<div class="job"><h3>Folders</h3>
    <p>Everything this app reads or writes. "Your data" is the one that
       matters — databases, settings and keybinds all live there, and it is
       not where the program is once you install it.</p>
    <div class="folders">${f.map(r=>`
      <div class="folderrow">
        <button class="openfolder" data-folder="${esc(r.key)}">open</button>
        <span class="foldername">${esc(r.label)}</span>
        ${r.note?`<span class="foldernote">${esc(r.note)}</span>`:''}
        <code class="folderpath">${esc(r.path)}</code>
      </div>`).join('')}</div>
    </div>
    <div class="job"><h3>Report a problem</h3>
    <p>Builds one zip holding your settings, the session logs — what the
       watcher read from every frame it looked at — and your recent debug
       screenshots. That is everything needed to work out why a contact was
       missed or a name came out wrong.</p>
    <p class="blurb">Your contact database, your watchlists and your API key
       are never in it. Full screenshots are, and a screenshot is your whole
       screen — the zip carries a MANIFEST.txt saying exactly what went in.
       Open it and look before you send it.</p>
    <div class="bundlerow">
      <button class="openfolder" id="bundle-go">make a support bundle</button>
      <button class="openfolder" id="bundle-text">text only, no screenshots</button>
    </div>
    <div class="out" id="bundle-out"></div>
    </div>`;
}

function diskHTML(){
  const d=data.stats.disk||[];
  if(!d.length) return '';
  return `<div class="disk">`+d.map(r=>
    `<span>${esc(r.name)} <b>${r.mb} MB</b> · ${r.files} file${r.files===1?'':'s'}</span>`
  ).join('')+`</div>`;
}

// Hold the input you want; the field fills in with its binding string.
//
// Polls /api/inputs rather than listening for browser key events, because the
// browser cannot see joystick buttons at all, and because the captured string
// has to be the one the watcher will parse.
//
// TWO THINGS THIS HAS TO GET RIGHT
//
// 1. A combination is pressed over TIME, not at once. Holding a modifier and
//    then hitting a button means the held set grows: {rctrl} then
//    {rctrl, joy3.b5}. Committing the first thing seen captures the modifier
//    alone. So the set is accumulated and only committed once it stops
//    growing, or once you start letting go.
//
// 2. The browser must not act on the keys. The settings tab is opened by a
//    <button>, which keeps focus - so a keypress arriving while detect is
//    listening can activate it and close the panel out from under you. Every
//    key event is swallowed at the capture phase until detect finishes.
let detectTimer=null, detectSwallow=null;

function swallow(ev){ ev.preventDefault(); ev.stopPropagation(); }

function stopDetect(msg){
  if(detectTimer){ clearInterval(detectTimer); detectTimer=null; }
  if(detectSwallow){
    for(const t of ['keydown','keyup','keypress'])
      window.removeEventListener(t, swallow, true);
    detectSwallow=null;
  }
  document.querySelectorAll('.detect.listening').forEach(b=>{
    b.classList.remove('listening'); b.textContent='detect';
  });
  if(msg) say(msg);
}

// Order the parts so the same gesture always produces the same string:
// modifiers first, then joystick buttons, then plain keys.
function sortParts(parts){
  const rank=x=>/^(l|r)(ctrl|shift|alt)$/.test(x)?0:(x.startsWith('joy')?1:2);
  return [...parts].sort((a,b)=>rank(a)-rank(b)||a.localeCompare(b));
}

async function startDetect(key, btn){
  stopDetect();
  btn.classList.add('listening');
  btn.textContent='press it…';
  // Nothing in the page should have focus while keys are flying.
  btn.blur();
  if(document.activeElement && document.activeElement.blur)
    document.activeElement.blur();
  for(const t of ['keydown','keyup','keypress'])
    window.addEventListener(t, swallow, true);
  detectSwallow=true;

  const input=document.getElementById('set_'+key);
  // Measured in MILLISECONDS, not polls: the browser throttles this interval
  // to roughly 300ms in practice rather than the 120ms asked for, so a
  // poll-count settle window silently means something different from what it
  // reads as.
  const SETTLE_MS=700;
  let tries=0, baseline=null, best=new Set(), steadySince=0, stableMap={};

  const commit=()=>{
    if(!best.size){ stopDetect('nothing detected'); return; }
    const parts=sortParts(best);
    const spec=parts.join('+');
    input.value=spec;
    input.dispatchEvent(new Event('change',{bubbles:true}));
    const alt=parts.map(p=>stableMap[p]||p).join('+');
    // A joy-to-key modifier shows up twice: the physical button AND the key it
    // emits. Requiring both is usually harmless, but if the emitted key is a
    // tap rather than a hold they will never be true together and the binding
    // will never fire. Worth saying so at capture time rather than in-flight.
    const hasJoy=parts.some(p=>p.startsWith('joy'));
    const plainKeys=parts.filter(p=>!p.startsWith('joy')&&
                                    !/^(l|r)(ctrl|shift|alt)$/.test(p));
    const warn = (parts.length>2 && hasJoy && plainKeys.length)
      ? `  — check it with:  python inputs.py --test "${spec}"` : '';
    stopDetect(`bound to ${spec}`+(alt!==spec?` — stable form: ${alt}`:'')+warn);
  };

  detectTimer=setInterval(async ()=>{
    if(++tries>250){ stopDetect('detect timed out'); return; }
    let j;
    try{ j=await fetch('/api/inputs',{cache:'no-store'}).then(r=>r.json()); }
    catch(e){ stopDetect('detect failed: '+e.message); return; }
    if(!j.ok){ stopDetect('detect failed: '+(j.error||'')); return; }

    const now=new Set([...(j.mods||[]),
                       ...(j.buttons||[]).map(b=>b.index),
                       ...(j.keys||[]).map(k=>k.toLowerCase())]);
    for(const b of (j.buttons||[])) stableMap[b.index]=b.stable;

    // Whatever was ALREADY held when you clicked detect is not what you are
    // binding - two of these sticks have reported button 1 as permanently down.
    if(baseline===null){
      baseline=now;
      if(now.size)
        say('ignoring '+[...now].join(', ')+' — already held when detect started');
      return;
    }
    const fresh=new Set([...now].filter(x=>!baseline.has(x)));

    // Compared by CONTENTS, not size: {rctrl, r} and {rctrl, joy0.b5} are both
    // size 2, so a size test reads a completely different gesture as "steady".
    const same = fresh.size===best.size && [...fresh].every(x=>best.has(x));
    if(!same && [...best].every(x=>fresh.has(x))){
      best=fresh; steadySince=Date.now();            // grew: still building
      btn.textContent='… '+sortParts(best).join('+');
      return;
    }
    if(!best.size){ best=fresh; steadySince=Date.now(); return; }
    if(!same){ commit(); return; }        // changed or shrank: that was it
    if(Date.now()-steadySince >= SETTLE_MS) commit();   // held steady
  },120);
}

// The ignore list, with a way back out. A filter you cannot see is a filter
// that will one day be silently suppressing a real player, and the hit count is
// how you notice: a rule with thousands of hits is doing more than you meant.
function ignoredHTML(){
  const rows=data.stats.ignored||[];
  if(!rows.length) return `<div class="job"><h3>Ignored labels</h3>
    <p>Nothing ignored. Open a contact that is not a person — a salvage crate,
       a mission marker — and use <b>not a player</b> in its window.</p></div>`;
  return `<div class="job"><h3>Ignored labels — ${rows.length}</h3>
    <p>These are never reported. Matching covers OCR variants of the same
       string, so one entry catches both spellings of a misread.</p>
    <div class="iglist">${rows.map(r=>`
      <div class="igrow">
        <span class="iglabel">${handleHTML(r.label,false)}</span>
        <span class="ignote">${esc(r.note||'')}</span>
        <span class="ighits">${r.hits} hit${r.hits===1?'':'s'}</span>
        <button class="igdel" data-unignore="${esc(r.label)}">un-ignore</button>
      </div>`).join('')}</div></div>`;
}

// A watchlist hit, drawn so it can never be mistaken for your own judgement.
//
// Everything about this block is deliberately not the house style of the rest
// of the tile: its own border, its own colour, the source named in the header
// rather than buried in prose. Your reports are yours; this is a stranger's
// claim, and a tile where the two look alike is a tile that will eventually
// get someone accused on evidence you never checked.
//
// A VARIANT match is drawn differently again. It does not mean this person is
// listed - it means a name that could OCR to theirs is. SLIVER and 5LIVER are
// two different people, so it says "could be", it never says "is", and it is
// dimmed so it does not read as a hit at a glance.
function watchHTML(c){
  const hits=c.watchlist||[];
  if(!hits.length) return '';
  return `<div class="wlflags">${hits.map(h=>{
    const variant = h.match==='variant';
    const unsure  = h.confidence==='unconfirmed';
    return `<div class="wlflag${variant?' wlvar':''}">
      <div class="wlhead">
        <span class="wlsrc">${variant?'could match':'listed on'} ${esc(h.source)}</span>
        ${unsure?`<span class="wltag">source unsure</span>`:''}
        ${variant?`<span class="wltag">not confirmed — lookalike spelling</span>`:''}
      </div>
      ${variant?`<div class="wlwhy">their list says <b>${esc(h.listed_as)}</b>,
         which could be an OCR misreading of this name — or a different
         person entirely</div>`:''}
      <div class="wlwhy">${h.claim?esc(h.claim)
        :'<i>no reason given on the list</i>'}</div>
      <div class="wlmeta">${h.by?'reported by '+esc(h.by):''}${
        h.posted_ts?' &middot; '+esc(h.posted_ts):''}${
        safeURL(h.url)?` &middot; <a href="${esc(safeURL(h.url))}" target="_blank"
          rel="noopener">source post</a>`:''}</div>
    </div>`;}).join('')}</div>`;
}

// Imported watchlists. Each row says WHOSE list it is, how many claims it
// makes, how many of those come with a stated reason, and when it was read.
//
// Reason coverage is on the row on purpose. A list where most entries are a
// bare name is a weaker instrument than one where most carry a reason, and
// that difference should be visible before you act on a flag rather than
// after.
function watchlistsHTML(){
  const rows=data.stats.watchlists||[];
  const orgs=data.stats.watchlist_orgs||[];
  const off=data.stats.watchlist_on===false;
  const list = !rows.length
    ? `<p class="blurb">No watchlists imported. These are lists other people
         keep — their claims stay theirs, shown as a separate line on the tile,
         and never counted in your score.</p>`
    : rows.map(s=>{
        const silenced=!s.enabled;
        return `<div class="orgrow${silenced?' wl-off':''}" data-wl="${s.id}">
          <div class="orghead">
            ${safeURL(s.url)?`<a class="org-name" target="_blank" rel="noopener"
                 href="${esc(safeURL(s.url))}">${esc(s.name)}</a>`
                  :`<span class="org-name">${esc(s.name)}</span>`}
            <span class="org-tag">${s.n} claim${s.n===1?'':'s'}</span>
            ${s.bundled?`<span class="org-tag wlship">came with sc-watch</span>`:''}
            <button class="orgpull" data-wl-toggle="${s.id}"
                    data-on="${silenced?'1':'0'}">${
              silenced?'use again':'silence'}</button>
            <button class="del" data-wl-forget="${s.id}">forget</button>
          </div>
          <div class="orgmeta">${
            silenced?'<b class="noroster">silenced — kept, but flagging nothing</b>'
                    :'active'} &middot; ${s.n_reason} of ${s.n} give a reason${
            s.synced_ts?' &middot; read '+ago(s.synced_ts):''}</div>
          ${s.note?`<div class="orgnote">${esc(s.note)}</div>`:''}
        </div>`;
      }).join('');

  return `<div class="job"><h3>Shared watchlists<span>${
      rows.length?' — '+rows.length:''}</span></h3>
    <p>Somebody else's judgement, kept in its own database. It never merges
       identities, never moves your score, and deleting a source deletes every
       claim it made — your own records do not notice.</p>
    ${off?`<div class="warn">Shared watchlists are switched OFF in
       Settings → Shared watchlists. Nothing below is flagging anything.</div>`:''}
    <div class="wllist">${list}</div>
    <div class="wlimport">
      <h4>Import a list</h4>
      <p class="blurb">Paste what you were given — RSI links, bare handles, or
         "handle - what they did", one per line. List markers and comment
         lines are ignored. Anything that is not a legal handle is shown to
         you rather than guessed at.</p>
      <div class="orgadd">
        <input id="wl-name" placeholder="name this list, e.g. griefer-log 2026"
               maxlength="80">
        <input id="wl-url" placeholder="where it came from (optional)" maxlength="300">
      </div>
      <textarea id="wl-text" rows="6" placeholder="SomeHandle - banned from an event
https://robertsspaceindustries.com/en/citizens/AnotherHandle
EXMPL-SomeName | camped a station pad"></textarea>
      <div class="orgadd">
        <label class="wl-unsure"><input type="checkbox" id="wl-unsure">
          the source was unsure about these</label>
        <button id="wl-preview">preview</button>
        <button id="wl-import" disabled>import</button>
      </div>
      <div class="out" id="wl-out"></div>
    </div>
    ${orgs.length?`<div class="orgmeta" style="margin-top:8px">Orgs named by
       these lists: ${orgs.map(o=>esc(o.sid)).join(', ')} — naming an org here
       does NOT flag it. Flag it yourself in the Orgs tab if you agree.</div>`:''}
    </div>`;
}

// ---- spoken alerts --------------------------------------------------------
// Drawn from audio.PHRASES on the server, so every line the voice can say is
// here and nothing is hand-listed twice. Fetched when the panel opens.
let speechData=null;

async function postRaw(url, payload){
  const r=await fetch(url,{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify(payload)});
  return r.json();
}
async function loadSpeech(){
  try{
    const j=await postRaw('/api/audio/phrases',{});
    speechData=j.ok?j.phrases:[];
    if(!j.ok) say('could not load phrases: '+(j.error||''));
  }catch(e){ speechData=[]; say('could not load phrases: '+e.message); }
  if(settingsOpen && settingsCat==='speech') renderSettings();
}
function speechHTML(){
  if(!speechData){ loadSpeech(); return '<div class="blurb">loading…</div>'; }
  const groups=[
    ['line','Whole lines',
     'What is said for each kind of alert. {distance} becomes “2.4 kilometres”, '
     +'or nothing at all when there is no range — a contact read from chat has none.'],
    ['reason','Reasons — what fills {reason} in “Known contact”',
     'Only one is said: the first of these that applies, in this order.'],
    ['flag','Org flags — what fills {flags} in “Lookup warning”',
     'One for each flag a lookup found, joined with commas. The same words '
     +'twice are said once.']];
  return groups.map(([g,title,blurb])=>`
    <div class="job"><h3>${esc(title)}</h3><p class="blurb">${esc(blurb)}${g==='line'?''
      :' Untick one to stop it announcing anybody: a contact with nothing else '
       +'switched on is then an ordinary contact, with the ordinary tone.'}</p>
      ${speechData.filter(p=>p.group===g).map(p=>`
      <div class="sprow" data-sp="${esc(p.key)}">
        <div class="sp-head"><b>${esc(p.title)}</b>
          <span class="sp-when">${esc(p.when)}</span></div>
        <div class="sp-edit">
          <input class="sp-in" maxlength="200" value="${esc(p.value)}"
                 placeholder="${esc(p.default||'blank — the tone plays alone')}">
          <button class="sp-hear" title="say it through this PC's voice">hear it</button>
          <button class="sp-default" ${p.value===p.default?'disabled':''}
                  title="back to: ${esc(p.default||'blank')}">default</button>
          <label class="sp-on" title="${p.group==='line'
            ?'off: the tone still plays, without words'
            :'off: this no longer announces anyone - they get the ordinary contact tone'}">
            <input type="checkbox" class="sp-chk" ${p.on?'checked':''}> on</label>
        </div>
        <div class="sp-ph">${p.on?'':'<b class="sp-off">off</b> · '}${p.placeholders.length
          ?'can use '+p.placeholders.map(x=>`<code>{${esc(x)}}</code>`).join(' ')
          :'no placeholders'}</div>
        <div class="sp-err" hidden></div>
      </div>`).join('')}
    </div>`).join('')+`
    <div class="sp-actions"><button id="sp-save">save phrases</button>
      <span class="blurb">Nothing changes until you save. “Hear it” plays the
        line as typed, saved or not.</span></div>`;
}

// ---- sharing --------------------------------------------------------------
// An export is a zip of YOUR records for someone you trust; an import shows
// what is in a file before anything is added. See share.py for what goes in
// and what never does.
let shareSources=null, sharePending=null, shareArmed=null;

function shareName(){ try{ return localStorage.getItem('sc-share-name')||''; }catch(e){ return ''; } }
async function loadShareSources(){
  try{ const j=await postRaw('/api/share/sources',{}); shareSources=j.ok?j.sources:[]; }
  catch(e){ shareSources=[]; }
  if(settingsOpen && settingsCat==='share') renderSettings();
}
function b64(buf){
  const bytes=new Uint8Array(buf); let s='';
  for(let i=0;i<bytes.length;i+=0x8000) s+=String.fromCharCode.apply(null, bytes.subarray(i,i+0x8000));
  return btoa(s);
}
function sharePreviewHTML(p){
  const when=new Date(p.exported_at*1000).toLocaleString();
  return `<div class="share-pv">
    <div class="share-pv-head">From <b>${esc(p.from)}</b>, exported ${esc(when)}${p.before
      ?` — replaces what you imported from ${esc(p.before.name)} ${ago(p.before.imported_at)}`:''}</div>
    <label><input type="checkbox" id="share-inc-reports" ${p.reports?'checked':'disabled'}>
      ${p.reports} report${p.reports===1?'':'s'} on ${p.players} player${p.players===1?'':'s'}${
      p.met.length?` — ${p.met.length} you have met`:''}${p.before
      ?` (${p.new_reports} new, ${p.updated_reports} updated${p.dropped_reports
        ?`, ${p.dropped_reports} they deleted will go`:''})`:''}</label>
    <label><input type="checkbox" id="share-inc-orgs" ${p.orgs?'checked':'disabled'}>
      ${p.orgs} flagged org${p.orgs===1?'':'s'}${p.new_orgs.length
        ?` — flags ${p.new_orgs.map(esc).join(', ')} here`:(p.orgs?' — all already flagged here':'')}</label>
    <label><input type="checkbox" id="share-inc-roster" ${p.roster?'checked':'disabled'}>
      ${p.roster} roster member${p.roster===1?'':'s'} they added by hand${p.roster_orphans
        ?` (${p.roster_orphans} on orgs not in this file and not flagged here — skipped)`:''}</label>
    <div class="share-act"><button id="share-import">import</button>
      <button id="share-cancel">cancel</button></div>
  </div>`;
}
function shareHTML(){
  if(shareSources===null) loadShareSources();
  return `
  <div class="job"><h3>Export for an org mate</h3>
    <p>A .zip of your own records for someone you trust: your reports (who,
       which reason, your note, when), the reasons they use, the orgs you
       flagged, and the roster members you added by hand. It leaves out where
       and when you played, screenshots, settings, your API key, and anything
       you imported from someone else.</p>
    <div class="orgadd">
      <input id="share-name" maxlength="40" value="${esc(shareName())}"
             placeholder="your in-game name — how your org knows you">
      <button id="share-export">export</button>
    </div>
    <p class="share-note">Shown beside everything you share, as “from &lt;name&gt;”.
       Your in-game name is best: it is the one your org mates will recognise.</p>
  </div>
  <div class="job"><h3>Import from an org mate</h3>
    <p>Pick a share file somebody sent you. You see what is in it before
       anything is added. Their reports show as “from &lt;their name&gt;” and
       count like your own; a newer file from the same person updates what you
       have from them rather than doubling it.</p>
    <input type="file" id="share-file" accept=".zip,application/zip">
    <div id="share-preview">${sharePending?sharePreviewHTML(sharePending.preview):''}</div>
  </div>
  <div class="job"><h3>Imported from</h3>
    ${shareSources===null?'<p>loading…</p>':shareSources.length?shareSources.map(s=>`
      <div class="share-src">
        <span><b>${esc(s.name)}</b> · imported ${ago(s.imported_at)} ·
          ${s.reports} report${s.reports===1?'':'s'} · ${s.roster} roster member${s.roster===1?'':'s'}</span>
        <button class="go" data-share-rm="${esc(s.install)}">${shareArmed===s.install
          ?'remove all of it? click again':'remove everything from '+esc(s.name)}</button>
      </div>`).join(''):'<p>Nothing imported yet.</p>'}
    <p class="share-note">Removing takes back their reports and roster
       members. Orgs their import flagged stay flagged — unflag those in the
       Orgs tab if you want to.</p>
  </div>`;
}

function renderSettings(){
  const schema=data.stats.settings_schema||[];
  if(!schema.length){ setPanel.innerHTML='<div class="blurb">settings unavailable</div>'; return; }
  const cats=[];
  schema.forEach(c=>{
    cats.push(c);
    if(c.key==='audio') cats.push({key:'speech',label:'Spoken alerts',
      blurb:'Every line the voice can say, and the pieces it builds them from. '
           +'Change any of them; the watcher picks up a saved change on its '
           +'next alert, no restart.',
      fields:[]});
  });
  cats.push(
    {key:'flags',label:'Report reasons',
     blurb:'The reasons you can file a report under, what colour each one is, '
          +'and whether it counts against a contact, for them, or not at all.',
     fields:[]},
    {key:'share',label:'Sharing',
     blurb:'Swap records with an org mate you trust: your reports, flagged orgs '
          +'and hand-added roster members, with whose they are kept on every one.',
     fields:[]},
    {key:'maintenance',label:'Maintenance',
     blurb:'One-off jobs. Each one shows you what it would do before it does it.',
     fields:[]});
  if(!cats.some(c=>c.key===settingsCat)) settingsCat=cats[0].key;

  setNav.innerHTML=cats.map(c=>
    `<button data-cat="${esc(c.key)}" aria-pressed="${c.key===settingsCat}">${esc(c.label)}</button>`
  ).join('');

  const cat=cats.find(c=>c.key===settingsCat);
  const showAdv=!!setVal('show_advanced');
  const fields=cat.fields.filter(f=>showAdv||!f.advanced);
  const hidden=cat.fields.length-fields.length;

  let body;
  if(cat.key==='share'){
    body=shareHTML();
  }else if(cat.key==='speech'){
    body=speechHTML();
  }else if(cat.key==='flags'){
    body=flagsHTML();
  }else if(cat.key==='maintenance'){
    body=diskHTML()+ignoredHTML()+JOBS.map(j=>`
      <div class="job" data-job="${j.id}">
        <h3>${esc(j.title)}</h3>
        <p>${esc(j.body)}</p>
        <button class="check" data-job="${j.id}">${j.direct?esc(j.verb):'check first'}</button>
        <div class="out"></div>
      </div>`).join('');
  }else{
    body=(cat.key==='storage'?diskHTML():'')+
      fields.map(f=>fieldHTML(f,setVal(f.key))).join('')+
      // After the retention controls, not before: the numbers above are the
      // argument for changing them, and the folders are what you go looking
      // at once you have.
      (cat.key==='storage'?foldersHTML():'')+
      (hidden?`<div class="adv-toggle">${hidden} advanced setting${hidden===1?'':'s'} hidden ·
        <button id="adv-on">show advanced</button></div>`:'')+
      // The sources live under their own switches rather than in a separate
      // category, so the toggle and the thing it toggles are on one screen.
      (cat.key==='watchlist'?watchlistsHTML():'')+
      (cat.key==='rsi'?keySettingsHTML():'');
  }
  setPanel.innerHTML=`<h2>${esc(cat.label)}</h2>
    <div class="blurb">${esc(cat.blurb||'')}</div>${body}`;
}

setNav.addEventListener('click', ev=>{
  const b=ev.target.closest('button[data-cat]');
  if(!b) return;
  settingsCat=b.dataset.cat;
  stopDetect();
  renderSettings();
});

// 'change' rather than 'input': saving on every keystroke would write a
// half-typed number, and the server clamps - so typing 120 into a field
// capped at 100 would fight the cursor back to 100 mid-word.
setPanel.addEventListener('change', async ev=>{
  const el=ev.target.closest('[data-key]');
  if(!el) return;
  const key=el.dataset.key;
  const v = el.type==='checkbox' ? el.checked
          : (el.type==='number' ? Number(el.value) : el.value);
  try{
    const j=await post('/api/settings',{values:{[key]:v}});
    if(!j.ok) throw new Error(j.error||'save failed');
    data.stats.settings=j.values;
    const f=(data.stats.settings_schema||[]).flatMap(c=>c.fields).find(x=>x.key===key);
    const shown=j.values[key];
    say(`saved ${f?f.label.toLowerCase():key} = ${shown}`+
        (f&&f.restart?` — restart the ${f.restart} to apply`:''));
    // Re-render only when the change alters what the form shows.
    applyTileSize();
    if(key==='show_advanced'){ renderSettings(); }
    else{
      const row=el.closest('.setrow');
      const isDefault=JSON.stringify(shown)===JSON.stringify(f.default);
      if(el.type!=='checkbox'&&String(shown)!==String(el.value)) el.value=shown;
      // The reset button's existence tracks "differs from default", so a save
      // that crosses that line has to add or remove it. Redrawing the whole
      // panel would steal focus from the control just used, so patch the row.
      if(row&&f&&(row.classList.contains('changed')!==!isDefault)){
        row.classList.toggle('changed',!isDefault);
        const old=row.querySelector('[data-reset]');
        if(isDefault){ if(old) old.remove(); }
        else if(!old){
          const b=document.createElement('button');
          b.className='reset'; b.dataset.reset=key;
          const d=f.type==='bool'?(f.default?'on':'off'):String(f.default);
          b.textContent=`reset · default ${d}`;
          row.firstElementChild.appendChild(b);
        }
      }
    }
  }catch(e){ say('could not save: '+e.message); }
});

setPanel.addEventListener('change', async ev=>{
  if(ev.target.id!=='share-file') return;
  const f=ev.target.files[0];
  if(!f) return;
  if(f.size>12*1024*1024){ say('that file is far too big to be a share'); return; }
  try{
    const data=b64(await f.arrayBuffer());
    const j=await postRaw('/api/share/preview',{data});
    if(!j.ok){ sharePending=null; say('cannot import that: '+(j.error||'')); }
    else sharePending={data, preview:j};
  }catch(e){ sharePending=null; say('cannot read that file: '+e.message); }
  renderSettings();
});
setPanel.addEventListener('input', ev=>{
  const inp=ev.target.closest('.sp-in');
  if(!inp) return;
  const row=inp.closest('.sprow');
  const p=(speechData||[]).find(x=>x.key===row.dataset.sp);
  if(p) row.querySelector('.sp-default').disabled=(inp.value===p.default);
});
setPanel.addEventListener('click', async ev=>{
  if(ev.target.id==='sk-save'){ saveKey('sk'); return; }
  if(ev.target.id==='sk-remove'){ saveKey('sk', true); return; }
  if(ev.target.id==='sk-restart'){
    try{ const j=await post('/api/restart-watcher',{}); say(j.message||'asked the watcher to restart'); }
    catch(e){ say('could not ask it to restart: '+e.message); }
    return;
  }
  if(ev.target.id==='share-export'){
    const name=(document.getElementById('share-name').value||'').trim();
    if(!name){ say('type the name to be shown as first — your in-game name is best'); return; }
    try{ localStorage.setItem('sc-share-name', name); }catch(e){}
    ev.target.disabled=true;
    try{
      const j=await postRaw('/api/share/export',{name});
      if(!j.ok) throw new Error(j.error||'failed');
      const bytes=Uint8Array.from(atob(j.data), ch=>ch.charCodeAt(0));
      const a=document.createElement('a');
      a.href=URL.createObjectURL(new Blob([bytes],{type:'application/zip'}));
      a.download=j.filename; document.body.appendChild(a); a.click();
      setTimeout(()=>{ URL.revokeObjectURL(a.href); a.remove(); }, 2000);
      say(`exported ${j.reports} report(s), ${j.orgs} org(s), ${j.roster} roster `
          +`member(s) as ${j.filename} — send that file to your org mate`);
    }catch(e){ say('export failed: '+e.message); }
    ev.target.disabled=false;
    return;
  }
  if(ev.target.id==='share-cancel'){ sharePending=null; renderSettings(); return; }
  if(ev.target.id==='share-import' && sharePending){
    const inc=k=>{ const el=document.getElementById('share-inc-'+k); return !!el&&el.checked; };
    ev.target.disabled=true;
    try{
      const j=await postRaw('/api/share/import',{data:sharePending.data,
        include:{reports:inc('reports'), orgs:inc('orgs'), roster:inc('roster')}});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message); sharePending=null; shareSources=null;
      await poll(); renderSettings();
    }catch(e){ say('import failed: '+e.message); ev.target.disabled=false; }
    return;
  }
  const srm=ev.target.closest('[data-share-rm]');
  if(srm){
    const id=srm.dataset.shareRm;
    if(shareArmed!==id){
      shareArmed=id; renderSettings();
      setTimeout(()=>{ if(shareArmed===id){ shareArmed=null;
        if(settingsOpen&&settingsCat==='share') renderSettings(); } }, ARM_MS);
      return;
    }
    shareArmed=null;
    try{
      const j=await postRaw('/api/share/remove',{install:id});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message); shareSources=null; await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); }
    return;
  }
  const hear=ev.target.closest('.sp-hear');
  if(hear){
    const row=hear.closest('.sprow');
    try{
      const j=await postRaw('/api/audio/preview',
        {key:row.dataset.sp, text:row.querySelector('.sp-in').value});
      say(j.ok ? (j.said?`saying: “${j.said}”`:(j.message||''))
               : ('cannot play it: '+(j.error||'')+(j.said?` — it would say “${j.said}”`:'')));
    }catch(e){ say('cannot play it: '+e.message); }
    return;
  }
  const dflt=ev.target.closest('.sp-default');
  if(dflt){
    const row=dflt.closest('.sprow');
    const p=(speechData||[]).find(x=>x.key===row.dataset.sp);
    if(p){ row.querySelector('.sp-in').value=p.default; dflt.disabled=true;
           say(`${p.title}: back to the default — save to keep it`); }
    return;
  }
  if(ev.target.id==='sp-save'){
    const values={}, muted=[];
    setPanel.querySelectorAll('.sprow').forEach(r=>{
      values[r.dataset.sp]=r.querySelector('.sp-in').value;
      if(!r.querySelector('.sp-chk').checked) muted.push(r.dataset.sp); });
    ev.target.disabled=true;
    try{
      const j=await postRaw('/api/audio/phrases',{values, muted});
      setPanel.querySelectorAll('.sprow').forEach(r=>{
        const err=(j.errors||{})[r.dataset.sp], box=r.querySelector('.sp-err');
        box.hidden=!err; box.textContent=err||'';
        r.classList.toggle('bad', !!err);
      });
      if(j.ok){ speechData=j.phrases;
        say('spoken phrases saved — the watcher uses them from its next alert'); }
      else say(j.error||'not saved');
    }catch(e){ say('not saved: '+e.message); }
    ev.target.disabled=false;
    return;
  }

  if(ev.target.id==='flags-save'){
    const btn=ev.target, out=document.getElementById('flags-out');
    btn.disabled=true;
    const rows=[...setPanel.querySelectorAll('.flagrow')].map(r=>({
      key: r.dataset.key || null,
      label: r.querySelector('.fl-label').value.trim(),
      color: r.querySelector('.fl-color').value,
      polarity: r.querySelector('.fl-pol').value,
      say: r.querySelector('.fl-say').value.trim(),
    }));
    // A blank row that never had a key is nothing at all - not a retirement,
    // not a new flag. Dropping it here keeps empty slots from becoming rows.
    const send=rows.filter(r=>r.key || r.label);
    for(const r of send){
      if(!r.key){
        // A new flag needs a key, and it has to be stable forever after. Made
        // from the first label typed, made unique, and never derived again -
        // rename later and the key stays put, which is the whole point.
        const base=(r.label.toLowerCase().replace(/[^a-z0-9]+/g,'_')
                    .replace(/^_+|_+$/g,'')||'flag').slice(0,28);
        let k=base, n=2;
        const taken=new Set(Object.keys(data.stats.flagmap||{})
          .concat(send.filter(x=>x.key).map(x=>x.key)));
        while(taken.has(k)){ k=base+'_'+(n++); }
        r.key=k;
      }
    }
    try{
      const j=await post('/api/flags',{flags:send});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message);
      // renderSettings() rebuilds the panel, so the message has to be written
      // AFTER it or it lands on a div that is about to be replaced.
      await poll(); renderSettings();
      const fresh=document.getElementById('flags-out');
      if(fresh) fresh.textContent=j.message;
    }catch(e){ out.textContent='failed: '+e.message; say('failed: '+e.message); }
    btn.disabled=false;
    return;
  }
  const bun=ev.target.closest('#bundle-go, #bundle-text');
  if(bun){
    const out=document.getElementById('bundle-out');
    const withFrames = bun.id==='bundle-go';
    bun.disabled=true;
    out.textContent='building… re-encoding screenshots, this takes a moment';
    try{
      const j=await post('/api/support-bundle',{frames:withFrames});
      if(!j.ok) throw new Error(j.error||'failed');
      // The manifest verbatim, not a summary of it. The point of the feature
      // is that nobody sends a file without knowing what is in it.
      out.textContent = j.manifest + '\n\nwritten: ' + j.file
        + '\nsize:    ' + j.mb + ' MB'
        + (j.images < j.images_available
            ? '\nnote:    ' + j.images + ' of ' + j.images_available
              + ' screenshots included (newest first)' : '');
      say('bundle ready — ' + j.mb + ' MB');
    }catch(e){ out.textContent='failed: '+e.message; say('failed: '+e.message); }
    bun.disabled=false;
    return;
  }
  const fol=ev.target.closest('[data-folder]');
  if(fol){
    fol.disabled=true;
    try{
      const j=await post('/api/open-folder',{key:fol.dataset.folder});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message);
    }catch(e){ say('failed: '+e.message); }
    // Re-enabled either way. The window opens behind the browser often
    // enough that the natural next move is to press it again.
    fol.disabled=false;
    return;
  }
  if(ev.target.id==='wl-preview' || ev.target.id==='wl-import'){
    const btn=ev.target, out=document.getElementById('wl-out');
    const text=(document.getElementById('wl-text').value||'').trim();
    if(!text){ out.textContent='paste something first'; return; }
    const isImport = btn.id==='wl-import';
    const name=(document.getElementById('wl-name').value||'').trim();
    if(isImport && !name){ out.textContent='give the list a name first'; return; }
    btn.disabled=true;
    try{
      const j=await post(isImport?'/api/watchlist/import':'/api/watchlist/preview',{
        text, name, url:(document.getElementById('wl-url').value||'').trim(),
        unsure: document.getElementById('wl-unsure').checked});
      if(!j.ok) throw new Error(j.error||'failed');
      if(isImport){
        say(j.message);
        // After the redraw, not before: renderSettings() replaces this div.
        await poll(); renderSettings();
        const fresh=document.getElementById('wl-out');
        if(fresh) fresh.textContent=j.message;
      }else{
        // The parse, in full, before a single row is written.
        //
        // Built as an array and joined rather than concatenated with newline
        // escapes: this whole page lives inside a Python string, and a
        // backslash-n written here has to survive Python's own escaping to
        // reach the browser. It did not - it arrived as a real newline and
        // broke every single-quoted string it was inside, which took the
        // entire settings panel down. Nothing to escape, nothing to lose.
        const out_lines = [j.n + ' handle(s) would be imported'];
        if(j.orgs.length) out_lines.push('orgs named: ' + j.orgs.join(', '));
        for(const e of j.entries)
          out_lines.push('  ' + e.handle + (e.claim ? '   ' + e.claim : ''));
        if(j.skipped_n){
          out_lines.push('');
          out_lines.push(j.skipped_n + ' line(s) NOT read as a handle:');
          for(const sk of j.skipped) out_lines.push('  ' + sk);
        }
        out.textContent = out_lines.join(String.fromCharCode(10));
        document.getElementById('wl-import').disabled = !(j.n||j.orgs.length);
      }
    }catch(e){ out.textContent='failed: '+e.message; }
    if(!isImport) btn.disabled=false; else btn.disabled=false;
    return;
  }
  const wlt=ev.target.closest('[data-wl-toggle]');
  if(wlt){
    wlt.disabled=true;
    try{
      const j=await post('/api/watchlist/enable',
        {id:Number(wlt.dataset.wlToggle), on:wlt.dataset.on==='1'});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message);
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); wlt.disabled=false; }
    return;
  }
  // Two clicks, same as unflag. Forgetting a source deletes every claim it
  // ever made, and there is no undo short of importing it again.
  const wlf=ev.target.closest('[data-wl-forget]');
  if(wlf){
    if(wlf.dataset.armed!=='1'){
      wlf.dataset.armed='1'; wlf.textContent='forget everything? click again';
      setTimeout(()=>{ if(wlf.isConnected){ wlf.dataset.armed='';
        wlf.textContent='forget'; } }, 4000);
      return;
    }
    wlf.disabled=true;
    try{
      const j=await post('/api/watchlist/forget',
        {id:Number(wlf.dataset.wlForget)});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message);
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); wlf.disabled=false; }
    return;
  }
  const un=ev.target.closest('[data-unignore]');
  if(un){
    un.disabled=true;
    try{
      const j=await post('/api/unignore',{label:un.dataset.unignore});
      if(!j.ok) throw new Error(j.error||'failed');
      say(`${un.dataset.unignore} will be reported again`);
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); un.disabled=false; }
    return;
  }
  const det=ev.target.closest('[data-detect]');
  if(det){
    if(det.classList.contains('listening')) stopDetect('detect cancelled');
    else startDetect(det.dataset.detect, det);
    return;
  }
  const rst=ev.target.closest('[data-reset]');
  if(rst){
    const key=rst.dataset.reset;
    const f=(data.stats.settings_schema||[]).flatMap(c=>c.fields)
             .find(x=>x.key===key);
    if(!f) return;
    try{
      const j=await post('/api/settings',{values:{[key]:f.default}});
      if(!j.ok) throw new Error(j.error||'failed');
      data.stats.settings=j.values;
      say(`reset ${f.label.toLowerCase()} to ${f.default}`+
          (f.restart?` — restart the ${f.restart} to apply`:''));
      renderSettings();
      applyTileSize();
    }catch(e){ say('could not reset: '+e.message); }
    return;
  }
  if(ev.target.id==='adv-on'){
    await post('/api/settings',{values:{show_advanced:true}});
    data.stats.settings.show_advanced=true;
    renderSettings();
    return;
  }
  const btn=ev.target.closest('button[data-job]');
  if(!btn) return;
  const wrap=btn.closest('.job'), out=wrap.querySelector('.out');
  const job=btn.dataset.job;
  const spec=JOBS.find(x=>x.id===job)||{};
  // A 'direct' job destroys nothing, so the confirm step would be ceremony.
  const apply=btn.classList.contains('go')||!!spec.direct;
  btn.disabled=true;
  out.textContent = apply?'working…':'checking…';
  try{
    const j=job==='restart'
      ? await post('/api/restart-watcher',{})
      : await post('/api/maintenance',{job, apply});
    if(!j.ok) throw new Error(j.error||'failed');
    let html=`<b>${esc(j.message||'done')}</b>`;
    if((j.items||[]).length)
      html+=`<div class="list">`+j.items.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    if((j.kept||[]).length)
      html+=`<div>held back, evidence would be lost:</div>`+
            `<div class="list">`+j.kept.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    if((j.errors||[]).length)
      html+=`<div class="list">`+j.errors.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    out.innerHTML=html;
    // A second, differently-worded button to actually do it. Nothing here
    // deletes on first click.
    if(!apply&&!spec.direct&&(j.count>0||job==='frames')){
      // (frames always offers the button: its dry run reports 0 when nothing
      //  is old enough yet, but the user may still want a sweep now.)
      const go=document.createElement('button');
      go.className='go'; go.dataset.job=job;
      go.textContent=(JOBS.find(x=>x.id===job)||{}).verb||'apply';
      out.appendChild(document.createTextNode(' '));
      out.appendChild(go);
    }
    if(apply){ await poll(); renderSettings(); }
  }catch(e){ out.textContent='failed: '+e.message; }
  btn.disabled=false;
});

// A render that was skipped used to be a render that never happened. The grid
// is only ever drawn from a stream frame, so bailing out because a text box had
// focus left the tile stale until some UNRELATED database write pushed the next
// frame - which is why a confirmed audit's amber button sat there afterwards.
// Now a skip is remembered and replayed once the reason for it is gone.
let renderPending=false;

function render(){
  settleTileLookups();
  // armedDel too, not just focus: arming a remove button changes nothing about
  // focus, so without this the 2s poll rebuilt the modal and silently disarmed
  // it - the second click then armed it again and it could never commit.
  if(openHandle && !lookupOpen && !editingInModal()) renderModal();
  grid.hidden=settingsOpen||orgsOpen;
  auditBar.hidden=!auditMode||settingsOpen||orgsOpen;
  setPane.hidden=!settingsOpen;
  orgPane.hidden=!orgsOpen;
  if(orgsOpen){ empty.hidden=true; applyTileSize(); renderStats();
    renderOrgs(); loadMembers(false); return; }
  if(settingsOpen){ empty.hidden=true; applyTileSize(); renderStats(); return; }
  if(editingInGrid() || !veil.hidden){ renderPending=true; return; }
  renderPending=false;
  const term=q.value.trim().toLowerCase();
  const onlyAlerts=fAlerts.getAttribute('aria-pressed')==='true';
  let list=auditMode ? auditList() : data.contacts.filter(c=>c.in_window!==false);
  // Not in the audit view: an uncertain read is worth settling whoever it is,
  // and "alerts only" left on from earlier would hide most of the queue.
  if(onlyAlerts && !auditMode) list=list.filter(c=>c.state==='known'||c.flags.length);
  if(term) list=list.filter(c=>c.handle.toLowerCase().includes(term)||
    c.orgs.some(o=>(o.sid+' '+o.name).toLowerCase().includes(term)));
  applyTileSize();
  grid.replaceChildren(...list.map(tile));
  renderAuditBar(list.length);
  empty.textContent=auditMode ? 'nothing to audit' : EMPTY_TEXT;
  empty.hidden=list.length>0;
  renderStats();
}

// ---- the audit view -----------------------------------------------------
// The header's "to audit" count is a way in, like "flagged orgs". It narrows
// the grid to the players with an uncertain read waiting, newest read first,
// including anyone who has dropped out of the normal window - the server sends
// those too, marked in_window:false, for exactly this.
let auditMode=false;
const auditBar=document.getElementById('auditbar');
const EMPTY_TEXT=empty.textContent;

function auditList(){
  const newest=c=>Math.max(...c.audits.map(a=>a.ts||0));
  return data.contacts.filter(c=>(c.audits||[]).length)
                      .sort((a,b)=>newest(b)-newest(a));
}

function setAuditMode(on){
  auditMode=on;
  if(on){
    if(settingsOpen){ settingsOpen=false; fSettings.setAttribute('aria-pressed','false'); }
    if(orgsOpen){ orgsOpen=false; fOrgs.setAttribute('aria-pressed','false'); }
  }
  render();
}

function renderAuditBar(shown){
  if(!auditMode) return;
  const all=data.contacts.filter(c=>(c.audits||[]).length);
  const reads=all.reduce((n,c)=>n+c.audits.length,0);
  const gone=data.stats.audits_orphaned||0;
  const hidden=all.length-shown;
  auditBar.innerHTML=`
    <span><b>to audit</b> — ${reads
      ? `${reads} uncertain read${reads===1?'':'s'} on ${all.length}
         player${all.length===1?'':'s'}. Open one, pick the right spelling or
         type it, then confirm or dismiss.`
      : 'nothing waiting on any contact.'}</span>
    ${hidden>0?`<span class="warn">${hidden} hidden by the search box</span>`:''}
    ${gone?`<span class="warn">${gone===1?'1 read belongs to a player'
                                          :gone+' reads belong to players'}
      no longer stored — "Delete old capture frames" in settings clears
      ${gone===1?'it':'them'}</span>`:''}
    <button class="filter" id="audit-done">show everyone</button>`;
}
auditBar.addEventListener('click', ev=>{
  if(ev.target.id==='audit-done') setAuditMode(false);
});
document.getElementById('s-audits').closest('.stat').addEventListener('click', ()=>{
  // From settings or orgs, the count opens the view rather than toggling off
  // one that is not on screen.
  setAuditMode(!(auditMode && !settingsOpen && !orgsOpen));
});

// Applied from the state so a change in another tab lands here too.
function applyTileSize(){
  const s=data.stats.settings||{};
  const px=Number(s.tile_size)||380;
  document.documentElement.style.setProperty('--tile', px+'px');
  document.body.classList.toggle('no-square', s.tile_square===false);
}

// ---- no API key ------------------------------------------------------------
// Said once, at the top, with what it costs and a box to fix it in. Built
// once rather than on every update, so a key half-pasted into it survives;
// only its visibility follows the data. "Not now" is remembered here, and
// Settings > RSI lookups keeps the same box for later.
const keyBar=document.getElementById('keybar');
function keyDismissed(){ try{ return localStorage.getItem('sc-keybar-off')==='1'; }catch(e){ return false; } }
const KEY_WHY=`Without a starcitizen-api.com key, players are looked up through
  SENTRY only: a misread handle cannot be confirmed as a misread (so no
  “no RSI account” audits), and an org's Piracy or Smuggling focus is only
  known if it is already cached — new orgs show “focus unknown”. Orgs, account
  age and flagged-org alerts still work. A key is free: register at
  <a href="https://starcitizen-api.com" target="_blank" rel="noopener">starcitizen-api.com</a>,
  copy it, and paste it here.`;
function keyFormHTML(prefix){
  return `<span class="keyform"><input id="${prefix}-in" type="password" autocomplete="off"
      maxlength="128" placeholder="paste your starcitizen-api.com key">
    <button id="${prefix}-save">save key</button></span>`;
}
function renderKeyBar(){
  const st=(data.stats||{}).api_key;
  const show=!!st && !st.set && !keyDismissed() && !settingsOpen;
  keyBar.hidden=!show;
  if(show && !keyBar.innerHTML){
    keyBar.innerHTML=`<div><b>No API key.</b> ${KEY_WHY}</div>
      <div class="keyrow">${keyFormHTML('kb')}
        <button id="kb-later" class="quiet">not now</button></div>`;
  }
}
function keySettingsHTML(){
  const st=(data.stats||{}).api_key||{};
  return `<div class="job"><h3>starcitizen-api.com key</h3>
    <p>${st.set
      ?`A key is set (ending <code>${esc(st.hint)}</code>, from ${st.from==='env'
        ?'the SC_API_KEY environment variable':'sc_api_key.txt'}).`
      :'<b class="noroster">No key set.</b> '+KEY_WHY}</p>
    <div class="keyrow">${keyFormHTML('sk')}
      ${st.file_set?'<button id="sk-remove" class="go">remove key</button>':''}</div>
    <p class="blurb">Saved as <code>${esc(st.path||'sc_api_key.txt')}</code>. The
      watcher reads it when it starts, so restart it afterwards.</p>
    <div id="sk-after"></div></div>`;
}
async function saveKey(prefix, remove){
  const inp=document.getElementById(prefix+'-in');
  const key=remove?'':(inp&&inp.value||'').trim();
  if(!remove && !key){ say('paste the key first'); return; }
  try{
    const j=await postRaw('/api/apikey', remove?{remove:true}:{key});
    if(!j.ok){ say(j.error||'not saved'); return; }
    if(inp) inp.value='';
    data.stats.api_key=j.status;
    say(j.message);
    renderKeyBar();
    if(settingsOpen) renderSettings();
    const after=document.getElementById('sk-after');
    if(after) after.innerHTML='<button id="sk-restart">restart the watcher now</button>';
  }catch(e){ say('not saved: '+e.message); }
}
keyBar.addEventListener('click', ev=>{
  if(ev.target.id==='kb-save') saveKey('kb');
  if(ev.target.id==='kb-later'){
    try{ localStorage.setItem('sc-keybar-off','1'); }catch(e){}
    keyBar.hidden=true;
    say('you can add a key any time: Settings > RSI lookups');
  }
});
keyBar.addEventListener('keydown', ev=>{ if(ev.key==='Enter'&&ev.target.id==='kb-in') saveKey('kb'); });

// An older install's data this copy is not using. 0.1.x kept it under
// %LOCALAPPDATA%; this zip keeps it beside the program - so the first run
// after an upgrade looks like an empty database. Only said, never moved.
const oldBar=document.getElementById('oldbar');
function renderOldBar(){
  const o=(data.stats||{}).old_data;
  let off=false;
  try{ off=localStorage.getItem('sc-oldbar-off')===(o&&o.old); }catch(e){}
  oldBar.hidden=!o||off||settingsOpen;
  if(!oldBar.hidden && !oldBar.innerHTML){
    oldBar.innerHTML=`<div><b>Your earlier data is not in use.</b> An older sc-watch
      kept its database in <code>${esc(o.old)}</code>, and this copy keeps its own in
      <code>${esc(o.here)}</code>. To bring it over: close both sc-watch windows,
      copy everything in the first folder into the second, and start sc-watch
      again. This note goes once it is carried over.</div>
      <div class="keyrow"><button id="ob-later">keep them separate</button></div>`;
  }
}
oldBar.addEventListener('click', ev=>{
  if(ev.target.id!=='ob-later') return;
  try{ localStorage.setItem('sc-oldbar-off', data.stats.old_data.old); }catch(e){}
  oldBar.hidden=true;
});

function renderStats(){
  const s=data.stats||{};
  s_players.textContent=s.players??'–'; s_sightings.textContent=s.sightings??'–';
  s_reports.textContent=s.reports??'–'; s_orgs.textContent=s.flagged_orgs??'–';
  s_pending.textContent=s.pending??'–';
  s_audits.textContent=s.audits??'–';
  s_audits.parentElement.style.color=s.audits?'var(--cyan)':'';
  s_audits.parentElement.classList.toggle('on', auditMode&&!settingsOpen&&!orgsOpen);
  renderKeyBar();
  renderOldBar();
  s_pending.parentElement.style.color=s.pending?'var(--amber)':'';
}

// Leaving a field is the moment a skipped render becomes safe. focusout fires
// before focus lands anywhere new, so the check is deferred a tick - otherwise
// tabbing between two inputs would redraw the grid out from under the cursor.
document.addEventListener('focusout', ()=>setTimeout(()=>{
  if(renderPending && !editingInGrid() && veil.hidden) render();
}, 0));
const s_players=document.getElementById('s-players'),
      s_sightings=document.getElementById('s-sightings'),
      s_reports=document.getElementById('s-reports'),
      s_orgs=document.getElementById('s-orgs'),
      s_pending=document.getElementById('s-pending'),
      s_audits=document.getElementById('s-audits'),
      updated=document.getElementById('updated');

async function poll(){
  try{
    const r=await fetch('/api/contacts',{cache:'no-store'});
    data=await r.json();
    if(data.error){say('error: '+data.error);}
    // Do not overwrite what an action just reported. Every handler used to set
    // a message and then call poll(), which replaced it with the clock before
    // it could be read - so a delete that worked looked like nothing happened.
    else if(!stickyUntil || Date.now()>stickyUntil){
      updated.textContent='updated '+new Date().toLocaleTimeString();}
    render();
  }catch(e){ say('disconnected — is ui_server.py running?'); }
}
// Push, not poll. Two reasons beyond the 80 MB/hour of identical JSON:
//
//  - Every render race this UI has had came from rebuilding the DOM on a timer
//    whether or not anything changed - wiped half-typed notes, closed the
//    category dropdown, disarmed confirm buttons. Rendering only on real change
//    removes the cause rather than guarding against it.
//  - setInterval is THROTTLED in a background tab, and this UI lives on a second
//    monitor behind a game. Measured during testing: the poll simply stopped and
//    the page went stale until it was fronted again. EventSource keeps delivering.
//
// poll() is kept for an immediate refresh after your own action, and as the
// fallback if the stream cannot connect.
let es=null;
function connect(){
  try{
    es=new EventSource('/api/stream');
  }catch(e){ setInterval(poll,2000); return; }
  es.onmessage=ev=>{
    try{ data=JSON.parse(ev.data); }catch(e){ return; }
    if(data.error){ say('error: '+data.error); return; }
    if(!stickyUntil || Date.now()>stickyUntil)
      updated.textContent='updated '+new Date().toLocaleTimeString();
    render();
  };
  es.onerror=()=>{
    // EventSource retries on its own; say so rather than looking frozen.
    if(es.readyState===EventSource.CLOSED)
      say('disconnected — is ui_server.py running?');
  };
}
poll().then(connect);


// ---- the Orgs view ----------------------------------------------------
// Flagged orgs used to be one category among many in Settings. They are now
// a view of their own, because an org is something you look INTO: who is on
// its roster, who has dropped off it, which of them you have already met,
// and which have a profile on file.
//
// Everyone listed here is stored locally the moment the roster is pulled, and
// that is the point of it: a member is flagged when you meet them whether or
// not the RSI lookup service is answering that day. Former members stay
// flagged too - people leave these orgs to act with fewer eyes on them.

function orgAddHTML(){
  return `<div class="orgadd">
    <input id="org-sid" placeholder="org SID, e.g. EXMPL" maxlength="32">
    <input id="org-note" placeholder="why (optional)" maxlength="200">
    <button id="org-flag">flag org</button>
    <div class="blurb">Flagging pulls its member list: one request a second,
      up to 20 pages, in the background.</div></div>`;
}

function renderOrgs(){
  const rows=data.stats.flagged_orgs_list||[];
  if(orgSel && !rows.some(o=>o.sid===orgSel)) orgSel=null;
  if(!orgSel && rows.length) orgSel=rows[0].sid;
  orgNavList.innerHTML = rows.length ? rows.map(o=>`
    <button data-org="${esc(o.sid)}" aria-pressed="${o.sid===orgSel}">
      <span class="on-name">${esc(o.name||o.sid)}</span>
      <span class="on-meta">${esc(o.sid)} · ${o.roster_ts
        ? o.roster_n+' member'+(o.roster_n===1?'':'s') : 'no member list'}${
        o.manual?' · '+o.manual+' by you':''}${
        o.manual_shared?' · '+o.manual_shared+' by org mates':''}${
        o.former?' · '+o.former+' former':''}</span>
    </button>`).join('')
    : `<div class="blurb" style="padding:12px 16px">No orgs flagged yet.</div>`;
  if(!orgNavAdd.innerHTML) orgNavAdd.innerHTML=orgAddHTML();
  renderOrgPanel(rows);
}

// The head and the member list are redrawn on every update; the add form only
// when the selected org changes, so what you are typing into it survives.
function renderOrgPanel(rows){
  const o=rows.find(r=>r.sid===orgSel);
  orgHead.innerHTML=orgPanelHTML(rows);
  orgMembers.innerHTML=o?orgMembersHTML(o, rows):'';
  if(!o){ orgMemAdd.innerHTML=''; orgMemAddFor=null; }
  else if(orgMemAddFor!==o.sid){ orgMemAdd.innerHTML=memAddHTML(o.sid); orgMemAddFor=o.sid; }
}

function memAddHTML(sid){
  return `<h3 class="mhead">add a member by hand</h3>
    <div class="orgadd memadd">
      <input id="mem-handle" placeholder="handle, spelled exactly as on RSI" maxlength="40">
      <input id="mem-note" placeholder="what you know (optional)" maxlength="200">
      <button id="mem-add" data-sid="${esc(sid)}">add to roster</button>
    </div>
    <div class="blurb">For people the public roster does not show — some orgs
      keep most of their members off it. They carry this org's flag like any
      member, whether or not you have met them yet, and stay until you remove
      them: a refresh never drops someone you added.</div>`;
}

// Fetched on selecting an org, and again whenever the stream says something
// changed - a lookup landing, a roster refreshed - at most every 1.5 s.
async function loadMembers(now){
  if(!orgsOpen || !orgSel) return;
  const wait = now ? 0 : Math.max(0, 1500-(Date.now()-orgFetchAt));
  clearTimeout(orgFetchTimer);
  orgFetchTimer=setTimeout(async ()=>{
    const sid=orgSel;
    orgFetchAt=Date.now();
    try{
      const j=await post('/api/org/members',{sid});
      if(j.ok){ orgData[sid]=j; if(orgsOpen && sid===orgSel)
        renderOrgPanel(data.stats.flagged_orgs_list||[]); }
    }catch(e){ /* the next stream frame tries again */ }
  }, wait);
}

function orgPanelHTML(rows){
  if(!rows.length) return `<h2>orgs</h2>
    <div class="blurb">Flag an org and every member of it is marked the moment
      you meet them — no report against each person, and no RSI lookup needed on
      the day. It records MEMBERSHIP, not blame.</div>`;
  const o=rows.find(r=>r.sid===orgSel); if(!o) return '';
  const d=orgData[o.sid];
  const job=(data.stats.org_jobs||{})[o.sid]||(d&&d.job)||null;
  const msg=(data.stats.rosters||{})[o.sid];
  const members=d?d.members:null;
  const cur=(members||[]).filter(m=>!m.gone_since);
  const former=(members||[]).filter(m=>m.gone_since);
  const need=cur.filter(m=>m.needs_lookup).length;
  const met=(members||[]).filter(m=>m.met>0).length;
  const flagged=new Set(rows.map(r=>r.sid));
  const armed=orgArmed===o.sid;
  const running=job&&job.running;
  return `
    <h2>${esc(o.name||o.sid)}</h2>
    <div class="blurb"><a class="org-name" target="_blank" rel="noopener"
       href="https://robertsspaceindustries.com/orgs/${encodeURIComponent(o.sid)}">${esc(o.sid)} on RSI</a>
       ${o.note?' · '+esc(o.note):''}</div>
    <div class="orgfacts">${o.roster_ts
        ? `<b>${o.roster_n}</b> on the roster, pulled ${ago(o.roster_ts)}`
        : `<b class="noroster">no member list pulled</b> — this flag reaches only players whose profile names it`}
      ${o.manual?` · <b>${o.manual}</b> added by you`:''}
      ${o.manual_shared?` · <b>${o.manual_shared}</b> added by org mates`:''}
      ${o.former?` · <b>${o.former}</b> former`:''}
      ${members?` · <b>${met}</b> you have met`:''}
      · reaches <b>${o.reaches}</b> of your contacts</div>
    <div class="orgactions">
      <button data-roster="${esc(o.sid)}">${o.roster_ts?'refresh members':'pull members'}</button>
      ${running
        ? `<button data-orgstop="${esc(o.sid)}">stop lookups</button>`
        : `<button data-orglookup="${esc(o.sid)}" ${need?'':'disabled'}>${need
            ? `look up ${need} member${need===1?'':'s'} missing a profile`
            : 'every member has a profile'}</button>`}
      <button class="del" data-unflag="${esc(o.sid)}">${armed
        ?(o.manual?`unflag, and forget its members — including the ${o.manual} you
          added? click again`:'unflag, and forget its members? click again')
        :'unflag'}</button>
    </div>
    ${msg?`<div class="orgmsg">${esc(msg)}</div>`:''}
    ${job?`<div class="orgmsg">${esc(job.message||'')}${running&&job.total
      ?` <span class="orgbar"><i style="width:${Math.round(100*job.done/job.total)}%"></i></span>`:''}</div>`:''}`;
}

function orgMembersHTML(o, rows){
  const d=orgData[o.sid];
  const members=d?d.members:null;
  const cur=(members||[]).filter(m=>!m.gone_since);
  const former=(members||[]).filter(m=>m.gone_since);
  const flagged=new Set(rows.map(r=>r.sid));
  return `
    ${members===null?`<div class="blurb">loading members…</div>`:`
      <h3 class="mhead">members <span>${cur.length}</span></h3>
      ${cur.length?`<div class="mtable">${cur.map(m=>memberRow(m,o.sid,flagged)).join('')}</div>`
        :`<div class="blurb">nobody on the roster${o.roster_ts?'':' yet — pull the member list'}</div>`}
      ${former.length?`
        <h3 class="mhead">former members <span>${former.length}</span></h3>
        <div class="blurb">Were on this roster and no longer are. Still flagged:
          people leave these orgs to act with fewer eyes on them. The roster
          omits hidden members, so dropping off it means they left <i>or</i> hid it.</div>
        <div class="mtable">${former.map(m=>memberRow(m,o.sid,flagged)).join('')}</div>`:''}`}`;
}

function memberRow(m, sid, flagged){
  const p=m.profile, chips=[];
  const age=d=>d<365?d+'d':(d/365).toFixed(1)+'y';
  if(m.met) chips.push(['ok', `met ×${m.met}`]);
  if(m.reports) chips.push(['bad', `${m.reports} report${m.reports===1?'':'s'}`]);
  if(!p) chips.push(m.lookup_failed?['warn','lookup failed']:['','not looked up']);
  else if(!p.exists) chips.push(['bad','no RSI account']);
  else{
    if(p.enlisted_days!=null) chips.push([p.enlisted_days<90?'warn':'', 'enlisted '+age(p.enlisted_days)]);
    (p.orgs||[]).filter(x=>x!==sid).forEach(x=>chips.push([flagged.has(x)?'bad':'', x]));
    if(p.age_days>30) chips.push(['warn', `profile ${p.age_days}d old`]);
  }
  const off=m.gone_since?`<span class="chip warn">off the roster since ${
    new Date(m.gone_since*1000).toLocaleDateString(undefined,{day:'numeric',month:'short'})} (left, or hid it)</span>`:'';
  if(m.manual_ts){
    const by=m.manual_by?'added by '+m.manual_by:'added by you';
    chips.unshift(['you', m.on_roster?by+' · on public roster':by]);
  }
  const sub=[m.rank, (m.display&&m.display.toUpperCase()!==m.handle)?m.display:'']
    .filter(Boolean).map(esc).join(' · ')+(m.note?`${m.rank||m.display?' · ':''}<i>${esc(m.note)}</i>`:'');
  const armed=orgArmedMember===sid+'/'+m.handle;
  return `<div class="mrow${m.gone_since?' former':''}">
    <span class="mh${m.met?' met':''}" data-member="${esc(m.handle)}" ${m.met
      ?'title="open your record of this contact"':''}>${esc(m.handle)}</span>
    <span class="msub">${sub}</span>
    <span class="mchips">${chips.map(([k,t])=>`<span class="chip ${k}">${esc(t)}</span>`).join('')}${off}</span>
    <span class="mact">
      <button class="lookupbtn" data-handle="${esc(m.handle)}"
        title="ask RSI now and show the answer">look up</button>
      <a href="https://robertsspaceindustries.com/citizens/${encodeURIComponent(m.handle)}"
         target="_blank" rel="noopener">RSI</a>
      ${m.manual_ts?`<button class="del${armed?' armed':''}" data-mrm="${esc(m.handle)}"
        data-sid="${esc(sid)}">${armed?'remove? click again':'remove'}</button>`:''}
    </span></div>`;
}

orgPane.addEventListener('click', async ev=>{
  const pick=ev.target.closest('[data-org]');
  if(pick){ orgSel=pick.dataset.org; orgArmed=null; renderOrgs(); loadMembers(true); return; }

  const lk=ev.target.closest('.lookupbtn');
  if(lk){ lookupContact(lk.dataset.handle); return; }
  const mh=ev.target.closest('.mh.met');
  if(mh){ openModal(mh.dataset.member); return; }

  if(ev.target.id==='mem-add'){
    const btn=ev.target, sid=btn.dataset.sid;
    const hEl=document.getElementById('mem-handle'), nEl=document.getElementById('mem-note');
    const handle=(hEl.value||'').trim();
    if(!handle){ say('type a handle first'); return; }
    btn.disabled=true;
    try{
      const j=await post('/api/org/member/add',{sid, handle, note:nEl.value});
      say(j.message); hEl.value=''; nEl.value=''; hEl.focus();
      await poll(); loadMembers(true);
    }catch(e){ say('failed: '+e.message); }
    btn.disabled=false;
    return;
  }
  const mrm=ev.target.closest('[data-mrm]');
  if(mrm){
    const key=mrm.dataset.sid+'/'+mrm.dataset.mrm;
    if(orgArmedMember!==key){
      orgArmedMember=key; renderOrgs();
      setTimeout(()=>{ if(orgArmedMember===key){ orgArmedMember=null; if(orgsOpen) renderOrgs(); } }, ARM_MS);
      return;
    }
    orgArmedMember=null;
    try{
      const j=await post('/api/org/member/remove',{sid:mrm.dataset.sid, handle:mrm.dataset.mrm});
      say(j.message); await poll(); loadMembers(true);
    }catch(e){ say('failed: '+e.message); }
    return;
  }

  if(ev.target.id==='org-flag'){
    const sid=(document.getElementById('org-sid').value||'').trim().toUpperCase();
    const note=(document.getElementById('org-note').value||'').trim();
    if(!sid){ say('type an org SID first'); return; }
    ev.target.disabled=true;
    try{
      const j=await post('/api/org/flag',{sid, note});
      say(j.ok?(j.message||'flagged '+sid):('failed: '+(j.error||'')));
      if(j.ok){ orgSel=sid; orgNavAdd.innerHTML=orgAddHTML(); }
      await poll(); loadMembers(true);
    }catch(e){ say('failed: '+e.message); }
    ev.target.disabled=false;
    return;
  }

  const pull=ev.target.closest('[data-roster]');
  if(pull){
    const sid=pull.dataset.roster;
    pull.disabled=true; pull.textContent='pulling…';
    try{
      const j=await post('/api/org/roster',{sid});
      say(j.message||j.error||'started');
      // Runs on a server thread. Success writes rows and the stream picks it
      // up; a failure writes nothing, so watch the status line as well.
      const before=(data.stats.rosters||{})[sid];
      for(let i=0;i<15;i++){
        await new Promise(r=>setTimeout(r,2000));
        if(!orgsOpen) break;
        await poll();
        const now=(data.stats.rosters||{})[sid];
        if(now && now!==before && !/pulling/.test(now)){ say(now); break; }
      }
      loadMembers(true);
    }catch(e){ say('failed: '+e.message); }
    return;
  }

  const lookup=ev.target.closest('[data-orglookup]');
  if(lookup){
    lookup.disabled=true;
    try{
      const j=await post('/api/org/lookup',{sid:lookup.dataset.orglookup});
      say(j.message||j.error||'started');
      await poll();
    }catch(e){ say('failed: '+e.message); lookup.disabled=false; }
    return;
  }
  const stop=ev.target.closest('[data-orgstop]');
  if(stop){
    stop.disabled=true;
    try{ const j=await post('/api/org/stop',{sid:stop.dataset.orgstop}); say(j.message||''); }
    catch(e){ say('failed: '+e.message); }
    return;
  }

  const unf=ev.target.closest('[data-unflag]');
  if(unf){
    const sid=unf.dataset.unflag;
    if(orgArmed!==sid){
      orgArmed=sid; renderOrgs();
      setTimeout(()=>{ if(orgArmed===sid){ orgArmed=null; if(orgsOpen) renderOrgs(); } }, 4000);
      return;
    }
    orgArmed=null;
    try{
      const j=await post('/api/org/unflag',{sid});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message); delete orgData[sid]; orgSel=null;
      await poll();
    }catch(e){ say('failed: '+e.message); }
    return;
  }
});
orgPane.addEventListener('keydown', ev=>{
  if(ev.key==='Enter' && (ev.target.id==='org-sid'||ev.target.id==='org-note'))
    document.getElementById('org-flag').click();
  if(ev.key==='Enter' && (ev.target.id==='mem-handle'||ev.target.id==='mem-note'))
    document.getElementById('mem-add').click();
});

// The header's "flagged orgs" count is a way in.
document.getElementById('s-orgs').closest('.stat').addEventListener('click', ()=>openOrgs());
