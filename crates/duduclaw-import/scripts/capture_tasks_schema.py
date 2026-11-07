"""Generate the reviewed task catalog without loading a legacy database."""
import pathlib,re,sqlite3
root=pathlib.Path('crates/duduclaw-gateway/src/task_store')
def literals(text):
    return [re.sub(r'\\\n\s*','',m.group(1)).replace('\\n','\n').replace('\\"','"').replace('\\\\','\\') for m in re.finditer(r'"((?:\\.|[^"\\])*)"',text)]
conn=sqlite3.connect(':memory:');statements=[]
def run(sql):
    try:conn.executescript(sql)
    except sqlite3.OperationalError as e:
        if str(e).startswith('duplicate column name:'):return
        raise
    statements.append(sql)
schema=(root/'schema.rs').read_text()
init=schema.split('fn init_schema(')[1].split('fn init_survival_evidence_schema')[0]
run(next(s for s in literals(init) if 'CREATE TABLE IF NOT EXISTS tasks (' in s))
def section(name,next_name):return schema.split('fn '+name+'(')[1].split('fn '+next_name+'(')[0]
def add_columns(text,table):
    migrations=text.split('let migrations:')[1].split('for (col, ddl)')[0]
    for col,ddl in re.findall(r'\(\s*"([^"\n]+)"\s*,\s*"([^"\n]+)"\s*,?\s*\)',migrations):run(f'ALTER TABLE {table} ADD COLUMN {ddl}')
dispatch=schema.split('fn add_dispatch_columns(')[1]
add_columns(dispatch,'tasks')
for sql in literals(dispatch):
    if sql.startswith(('CREATE ',)):run(sql)
access=(root/'access.rs').read_text().split('fn init_task_access_schema(')[1]
run(next(s for s in literals(access) if s.startswith('CREATE TABLE IF NOT EXISTS task_access_principals')))
authority=(root/'authority.rs').read_text();columns=literals(authority.split('const AUTHORITY_COLUMNS:')[1].split('];')[0])
for sql in literals(authority.split('fn init_authority_schema(')[1]):
    if sql.startswith('CREATE TABLE IF NOT EXISTS task_authority_epochs'):run(sql)
trigger=next(s for s in literals(authority) if s.startswith('DROP TRIGGER IF EXISTS task_authority_revision_v1'))
run(trigger.replace('{}',','.join(columns+['status','claimed_by'])).replace('{edits}',' OR '.join('OLD.'+c+' IS NOT NEW.'+c for c in columns)))
for name,next_name in [('init_plan_schema','add_dispatch_columns'),('init_iteration_schema','init_plan_schema'),('init_survival_evidence_schema','init_iteration_schema')]:
    text=section(name,next_name)
    for sql in literals(text):
        if sql.startswith(('CREATE ','ALTER ')) and '{' not in sql:run(sql)
    if name=='init_iteration_schema':add_columns(text,'task_iterations')
resp=(root/'resp_schema.rs').read_text()
run(next(s for s in literals(resp) if s.lstrip().startswith('CREATE TABLE IF NOT EXISTS responsibility_schema_meta')))
for sql in literals(resp.split('pub(super) fn init_responsibility_schema')[1]):
    if sql.startswith(('CREATE ','ALTER ')) and '{' not in sql:run(sql)
assign=(root/'assign_schedules.rs').read_text();run(next(s for s in literals(assign) if s.lstrip().startswith('CREATE TABLE IF NOT EXISTS assign_schedules')))
a2a=(root/'a2a.rs').read_text();run(next(s for s in literals(a2a) if s.startswith('CREATE TABLE IF NOT EXISTS a2a_requests')))
pathlib.Path('crates/duduclaw-import/src/tasks-v1793.sql').write_text('-- Generated from reviewed task-store DDL.\n'+'\n'.join(s.strip()+';' if not s.rstrip().endswith(';') else s.strip() for s in statements)+'\n')
print('schema objects:',conn.execute('SELECT count(*) FROM sqlite_schema').fetchone()[0])
