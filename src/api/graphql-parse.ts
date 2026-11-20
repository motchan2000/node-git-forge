// A GraphQL document lexer and parser: operations, fragments, variables with defaults, aliases,
// arguments (variables, scalars, enums, lists, input objects), directives, inline fragments.

export type ValueNode =
  | { kind: 'var'; name: string }
  | { kind: 'int'; value: number }
  | { kind: 'float'; value: number }
  | { kind: 'string'; value: string }
  | { kind: 'bool'; value: boolean }
  | { kind: 'null' }
  | { kind: 'enum'; value: string }
  | { kind: 'list'; values: ValueNode[] }
  | { kind: 'object'; fields: Record<string, ValueNode> };

export interface Directive {
  readonly name: string;
  readonly args: Record<string, ValueNode>;
}

export type Selection =
  | { kind: 'field'; alias: string | null; name: string; args: Record<string, ValueNode>; directives: Directive[]; selections: Selection[] | null }
  | { kind: 'spread'; name: string; directives: Directive[] }
  | { kind: 'inline'; typeCondition: string | null; directives: Directive[]; selections: Selection[] };

export interface VariableDef {
  readonly name: string;
  readonly type: string;
  readonly defaultValue: ValueNode | null;
}

export interface Operation {
  readonly kind: 'operation';
  readonly operation: 'query' | 'mutation' | 'subscription';
  readonly name: string | null;
  readonly variables: VariableDef[];
  readonly selections: Selection[];
}

export interface Fragment {
  readonly kind: 'fragment';
  readonly name: string;
  readonly typeCondition: string;
  readonly selections: Selection[];
}

export interface Document {
  readonly operations: Operation[];
  readonly fragments: Map<string, Fragment>;
}

export class GraphQLSyntaxError extends Error {}

type Token = { t: 'punct' | 'name' | 'int' | 'float' | 'string' | 'eof'; v: string; pos: number };

function lex(src: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i] as string;
    if (c === ' ' || c === '\t' || c === '\n' || c === '\r' || c === ',' || c === '﻿') {
      i += 1;
      continue;
    }
    if (c === '#') {
      while (i < n && src[i] !== '\n' && src[i] !== '\r') i += 1;
      continue;
    }
    if (c === '.' && src.startsWith('...', i)) {
      tokens.push({ t: 'punct', v: '...', pos: i });
      i += 3;
      continue;
    }
    if ('!$&()[]{}:=@|'.includes(c)) {
      tokens.push({ t: 'punct', v: c, pos: i });
      i += 1;
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1;
      while (j < n && /[A-Za-z0-9_]/.test(src[j] as string)) j += 1;
      tokens.push({ t: 'name', v: src.slice(i, j), pos: i });
      i = j;
      continue;
    }
    if (c === '-' || /[0-9]/.test(c)) {
      const m = /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/.exec(src.slice(i));
      if (m === null) throw new GraphQLSyntaxError(`Syntax Error: unexpected character ${JSON.stringify(c)} at ${i}`);
      tokens.push({ t: m[2] !== undefined || m[3] !== undefined ? 'float' : 'int', v: m[0], pos: i });
      i += m[0].length;
      continue;
    }
    if (c === '"') {
      if (src.startsWith('"""', i)) {
        const end = src.indexOf('"""', i + 3);
        if (end < 0) throw new GraphQLSyntaxError('Syntax Error: unterminated block string');
        tokens.push({ t: 'string', v: blockString(src.slice(i + 3, end)), pos: i });
        i = end + 3;
        continue;
      }
      let j = i + 1;
      let out = '';
      while (j < n && src[j] !== '"') {
        if (src[j] === '\n') throw new GraphQLSyntaxError('Syntax Error: unterminated string');
        if (src[j] === '\\') {
          const e = src[j + 1];
          if (e === 'u') {
            out += String.fromCharCode(parseInt(src.slice(j + 2, j + 6), 16));
            j += 6;
            continue;
          }
          const map: Record<string, string> = { '"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' };
          out += map[e ?? ''] ?? (e ?? '');
          j += 2;
          continue;
        }
        out += src[j];
        j += 1;
      }
      if (j >= n) throw new GraphQLSyntaxError('Syntax Error: unterminated string');
      tokens.push({ t: 'string', v: out, pos: i });
      i = j + 1;
      continue;
    }
    throw new GraphQLSyntaxError(`Syntax Error: unexpected character ${JSON.stringify(c)} at ${i}`);
  }
  tokens.push({ t: 'eof', v: '', pos: n });
  return tokens;
}

function blockString(raw: string): string {
  const lines = raw.replace(/\\"""/g, '"""').split(/\r\n|\n|\r/);
  let common = Infinity;
  for (let k = 1; k < lines.length; k += 1) {
    const line = lines[k] as string;
    const indent = line.length - line.trimStart().length;
    if (indent < line.length) common = Math.min(common, indent);
  }
  const out = lines.map((l, k) => (k === 0 || common === Infinity ? l : l.slice(common)));
  while (out.length > 0 && (out[0] as string).trim() === '') out.shift();
  while (out.length > 0 && (out[out.length - 1] as string).trim() === '') out.pop();
  return out.join('\n');
}

class Parser {
  private i = 0;
  private readonly tokens: Token[];
  constructor(src: string) {
    this.tokens = lex(src);
  }

  private peek(): Token {
    return this.tokens[this.i] as Token;
  }
  private next(): Token {
    return this.tokens[this.i++] as Token;
  }
  private is(v: string): boolean {
    const t = this.peek();
    return t.t === 'punct' && t.v === v;
  }
  private expect(v: string): void {
    const t = this.next();
    if (t.t !== 'punct' || t.v !== v) throw new GraphQLSyntaxError(`Syntax Error: expected "${v}", found ${t.t === 'eof' ? '<EOF>' : JSON.stringify(t.v)}`);
  }
  private name(): string {
    const t = this.next();
    if (t.t !== 'name') throw new GraphQLSyntaxError(`Syntax Error: expected a name, found ${t.t === 'eof' ? '<EOF>' : JSON.stringify(t.v)}`);
    return t.v;
  }

  document(): Document {
    const operations: Operation[] = [];
    const fragments = new Map<string, Fragment>();
    while (this.peek().t !== 'eof') {
      if (this.is('{')) {
        operations.push({ kind: 'operation', operation: 'query', name: null, variables: [], selections: this.selectionSet() });
        continue;
      }
      const word = this.name();
      if (word === 'fragment') {
        const name = this.name();
        if (this.name() !== 'on') throw new GraphQLSyntaxError('Syntax Error: expected "on"');
        const typeCondition = this.name();
        this.directives();
        fragments.set(name, { kind: 'fragment', name, typeCondition, selections: this.selectionSet() });
        continue;
      }
      if (word !== 'query' && word !== 'mutation' && word !== 'subscription') throw new GraphQLSyntaxError(`Syntax Error: unexpected ${JSON.stringify(word)}`);
      let name: string | null = null;
      if (this.peek().t === 'name') name = this.name();
      const variables: VariableDef[] = [];
      if (this.is('(')) {
        this.next();
        while (!this.is(')')) {
          this.expect('$');
          const vname = this.name();
          this.expect(':');
          const type = this.typeRef();
          let defaultValue: ValueNode | null = null;
          if (this.is('=')) {
            this.next();
            defaultValue = this.value(true);
          }
          this.directives();
          variables.push({ name: vname, type, defaultValue });
        }
        this.next();
      }
      this.directives();
      operations.push({ kind: 'operation', operation: word, name, variables, selections: this.selectionSet() });
    }
    if (operations.length === 0) throw new GraphQLSyntaxError('Syntax Error: the document has no operation');
    return { operations, fragments };
  }

  private typeRef(): string {
    let out: string;
    if (this.is('[')) {
      this.next();
      out = `[${this.typeRef()}]`;
      this.expect(']');
    } else {
      out = this.name();
    }
    if (this.is('!')) {
      this.next();
      out += '!';
    }
    return out;
  }

  private directives(): Directive[] {
    const out: Directive[] = [];
    while (this.is('@')) {
      this.next();
      const name = this.name();
      out.push({ name, args: this.is('(') ? this.arguments() : {} });
    }
    return out;
  }

  private arguments(): Record<string, ValueNode> {
    this.expect('(');
    const args: Record<string, ValueNode> = {};
    while (!this.is(')')) {
      const name = this.name();
      this.expect(':');
      args[name] = this.value(false);
    }
    this.next();
    return args;
  }

  selectionSet(): Selection[] {
    this.expect('{');
    const out: Selection[] = [];
    while (!this.is('}')) {
      if (this.is('...')) {
        this.next();
        if (this.peek().t === 'name' && this.peek().v !== 'on') {
          const name = this.name();
          out.push({ kind: 'spread', name, directives: this.directives() });
          continue;
        }
        let typeCondition: string | null = null;
        if (this.peek().t === 'name' && this.peek().v === 'on') {
          this.next();
          typeCondition = this.name();
        }
        const directives = this.directives();
        out.push({ kind: 'inline', typeCondition, directives, selections: this.selectionSet() });
        continue;
      }
      let alias: string | null = null;
      let name = this.name();
      if (this.is(':')) {
        this.next();
        alias = name;
        name = this.name();
      }
      const args = this.is('(') ? this.arguments() : {};
      const directives = this.directives();
      const selections = this.is('{') ? this.selectionSet() : null;
      out.push({ kind: 'field', alias, name, args, directives, selections });
    }
    this.next();
    if (out.length === 0) throw new GraphQLSyntaxError('Syntax Error: empty selection set');
    return out;
  }

  private value(constant: boolean): ValueNode {
    const t = this.peek();
    if (t.t === 'punct' && t.v === '$') {
      if (constant) throw new GraphQLSyntaxError('Syntax Error: a variable is not allowed here');
      this.next();
      return { kind: 'var', name: this.name() };
    }
    if (t.t === 'int') {
      this.next();
      return { kind: 'int', value: Number(t.v) };
    }
    if (t.t === 'float') {
      this.next();
      return { kind: 'float', value: Number(t.v) };
    }
    if (t.t === 'string') {
      this.next();
      return { kind: 'string', value: t.v };
    }
    if (t.t === 'name') {
      this.next();
      if (t.v === 'true' || t.v === 'false') return { kind: 'bool', value: t.v === 'true' };
      if (t.v === 'null') return { kind: 'null' };
      return { kind: 'enum', value: t.v };
    }
    if (t.t === 'punct' && t.v === '[') {
      this.next();
      const values: ValueNode[] = [];
      while (!this.is(']')) values.push(this.value(constant));
      this.next();
      return { kind: 'list', values };
    }
    if (t.t === 'punct' && t.v === '{') {
      this.next();
      const fields: Record<string, ValueNode> = {};
      while (!this.is('}')) {
        const name = this.name();
        this.expect(':');
        fields[name] = this.value(constant);
      }
      this.next();
      return { kind: 'object', fields };
    }
    throw new GraphQLSyntaxError(`Syntax Error: unexpected ${t.t === 'eof' ? '<EOF>' : JSON.stringify(t.v)}`);
  }
}

export function parseDocument(src: string): Document {
  return new Parser(src).document();
}

/** Resolve a value node against the request's variables. */
export function valueOf(node: ValueNode, variables: Readonly<Record<string, unknown>>): unknown {
  switch (node.kind) {
    case 'var':
      return variables[node.name] ?? null;
    case 'int':
    case 'float':
    case 'string':
    case 'bool':
    case 'enum':
      return node.value;
    case 'null':
      return null;
    case 'list':
      return node.values.map((v) => valueOf(v, variables));
    case 'object': {
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(node.fields)) out[k] = valueOf(v, variables);
      return out;
    }
  }
}
