{ path }:

let
  lib = import <nixpkgs/lib>;
  yaml = builtins.readFile path;

  forEachLine = f: file: let
    split = lib.splitString "\n" file;
    applied = lib.imap f split;
    joined = lib.concatStringsSep "\n" applied;
  in
    applied /*joined*/;

  replaced = forEachLine (i: line:
    let
      m = builtins.match "( *)(.*)" line;
      spaces = lib.stringLength (lib.head m);
    in {
      lineNumber = i;
      indent = if lib.mod spaces 2 != 0 then throw "indentation not modulo 2" else spaces / 2;
      content = lib.elemAt m 1;
    }
  ) yaml;

  parse = lines: let
    blocks = lib.foldl' (acc: line:
      let
        endOfBlock = line.indent == 0;
      in {
        lastIndent = line.indent;
        blocks = if endOfBlock then
          acc.blocks ++ [
            (lib.mapNullable (currentBlock: currentBlock // { value = if lib.isList currentBlock.value then parse currentBlock.value else currentBlock.value; }) acc.currentBlock)
          ]
        else
          acc.blocks;
        currentBlock = if endOfBlock then
          let
            blockBegin = builtins.match "\"?([^\"]*)\"?:" line.content;
            singleAttr = builtins.match "\"?([^\"]*)\"?: \"?([^\"]*)\"?" line.content;
          in if blockBegin != null then {
            name = lib.head blockBegin;
            value = [];
          } else if singleAttr != null then {
            name = lib.head singleAttr;
            value = lib.last singleAttr;
          } else null
        else if acc.currentBlock != null then {
          inherit (acc.currentBlock) name;
          value = acc.currentBlock.value ++ [
            (line // {
              indent = line.indent - 1;
            })
          ];
        } else throw "line no ${line.lineNumber}";
      }
    ) 
    {
      lastIndent = 0;
      blocks = [];
      currentBlock = null;
    }
    lines;
  in lib.listToAttrs (lib.filter lib.isAttrs (blocks.blocks ++ [blocks.currentBlock]));

  /*replaced = lib.pipe yaml [
    (forEachLine (line: let
      m = builtins.match "\"?(.*)\"?:" line;
    in
      if isNull m || lib.any isNull m
        then line
      else
        lib.concatStringsSep ":" (map (x: "\"${x}\"") m)
    ))
    (forEachLine (line: let
      m = builtins.match "\"?(.*)\"?: \"?(.*)\"?" line;
    in
      if isNull m || lib.any isNull m
        then line
      else
        lib.concatStringsSep ":" (map (x: "\"${x}\"") m)
    ))
  ];*/

in
  parse replaced
