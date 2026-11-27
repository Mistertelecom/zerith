---
name: blender-engineering
description: "Use ao modelar peças 3D paramétricas, conjuntos mecânicos, renders de engenharia ou exportações CAD/impressão 3D (STL, 3MF, GLB) no Blender com a ferramenta `blender`. Garante escala em milímetros, constantes paramétricas, tolerâncias de manufatura, validação manifold/colisão (`cad_check`) e inspeção visual em 4 vistas (`render_views`)."
category: software-development
tags: blender 3d cad bmesh stl 3mf glb engenharia parametrico impressao-3d render
---

# Engenharia 3D Paramétrica e CAD no Blender

## 1. Fluxo obrigatório de modelagem e validação
Prefira **`blender{action: "batch", steps: [...]}`** para executar construção, validação geométrica, inspeção visual e exportação em um único ciclo coeso (ou em poucas chamadas):

1. **`status`**: verifique binário do Blender detectado e se o bridge ao vivo (`127.0.0.1:9877` autenticado por token) está ativo; sem bridge, o modo `auto` usa `blender --background --python` automaticamente.
2. **`exec_bpy`**: construa a geometria parametricamente em Python (`bpy`, `bmesh`, `mathutils`, `math` já importados).
3. **`cad_check`**: valide estanqueidade (`is_manifold`), arestas de borda, faces degeneradas, normais consistentes, escala aplicada (`(1, 1, 1)`) e colisões/sobreposições entre peças (`BVHTree`).
4. **`render_views`**: gere o grid 2×2 ortográfico/isométrico (`front`, `right`, `top`, `iso_45`), que anexa o PNG automaticamente para revisão visual.
5. **Corrija** qualquer interseção, face aberta, parede fina ou desalinhamento visível e repita `cad_check` + `render_views` antes de finalizar.
6. **`export`**: exporte para `stl`, `3mf`, `glb`, `gltf`, `obj` ou `fbx` com `apply_transforms: true`.

## 2. Disciplina de unidades e escala em milímetros
- Padronize a cena no início do script paramétrico:
  ```python
  scene = bpy.context.scene
  scene.unit_settings.system = 'METRIC'
  scene.unit_settings.scale_length = 0.001  # 1 Blender Unit = 1 mm
  ```
- Com `scale_length = 0.001`, todas as coordenadas e dimensões em `bpy` ficam diretamente em **milímetros** (evita peça 1000× maior ou menor no fatiador).
- Sempre aplique rotação e escala antes de medir ou exportar:
  ```python
  bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)
  ```
- Confira medidas reais, volume (`volume_mm3`, `volume_cm3`) e massa estimada (`PLA`, `PETG`, `ABS`, `Nylon`, `Aluminum_6061`, `Steel_304`) via `inspect_object`.

## 3. Modelagem paramétrica e `bmesh` limpo
- **Constantes no topo do script:** declare todas as cotas críticas no início (`WALL_MM`, `HOLE_DIAM_MM`, `TOLERANCE_MM`, `FILLET_MM`, `BASE_H_MM`) e derive posições por aritmética explícita — nunca números mágicos espalhados.
- **Tolerâncias para impressão 3D e encaixes:**
  - Folga deslizante / encaixe livre (*slip fit*): `0.20`–`0.30 mm` por lado (ou `+0.3`–`0.5 mm` no diâmetro de furos passantes).
  - Encaixe sob pressão (*press fit*): `0.05`–`0.10 mm`.
  - Rosca / inserto térmico: siga o diâmetro nominal do inserto (ex.: M3 = furo `4.0`–`4.2 mm` conforme datasheet).
- **Operações booleanas sem artefatos:**
  - Prefira `mod.solver = 'EXACT'` (ou `'FLOAT'` apenas quando `'EXACT'` tiver faces coplanares problemáticas).
  - **Evite faces coplanares e Z-fighting:** estenda cortadores booleanos em `0.2`–`1.0 mm` além das faces externas da peça alvo para garantir corte limpo sem películas residuais.
  - Aplique os modificadores (`bpy.ops.object.modifier_apply`) e limpe os objetos cortadores auxiliares (`bpy.data.objects.remove(cutter, do_unlink=True)`) antes do `cad_check`.
- **Chanfros e filetes (`bmesh.ops.bevel` / modificador `BEVEL`):**
  - Use `clamp_overlap = True` e raio menor que metade da menor aresta adjacente; verifique no `cad_check` se nenhum bevel gerou faces de área zero ou arestas não-manifold.

## 4. Checklist DfM (Design for Manufacturing)
Antes de considerar uma peça pronta para fabricação ou impressão:
- [ ] **Estanqueidade (`is_manifold == true`):** `non_manifold_edges == 0` e `boundary_edges == 0`.
- [ ] **Geometria limpa:** `zero_area_faces == 0`, sem normais invertidas ou inconsistentes entre faces adjacentes.
- [ ] **Escala aplicada:** `scale_applied == true` (`(1, 1, 1)` em todos os eixos).
- [ ] **Sem colisões indesejadas:** `cad_check` com `overlaps == []` entre subpeças que não deveriam se interpenetrar.
- [ ] **Espessura mínima de parede:** respeite o processo (`>= 1.2 mm` / 3 perímetros em FDM bico 0.4 mm; `>= 0.8 mm` em SLA; `>= 1.5 mm` em alumínio CNC).
- [ ] **Overhangs (> 45°):** evite pontes longas ou inclinações maiores que `45°` sem chanfro de suporte, ou oriente a face plana principal em `Z = 0`.

## 5. Revisão visual obrigatória (`render_views`)
- Após `exec_bpy` + `cad_check`, rode `render_views` (padrão `engine: "workbench"`, grid 2×2 com `front`, `right`, `top`, `iso_45`).
- Inspecione a imagem anexada procurando:
  1. Furos desalinhados ou obstruídos por cortadores não removidos.
  2. Interferência/penetração entre peças montadas.
  3. Paredes finas demais, cantos vivos sem chanfro de alívio ou flutuantes fora do plano de apoio (`Z = 0`).
- Ao corrigir, rode novamente `batch` (`exec_bpy` → `cad_check` → `render_views` → `export`) e só entregue os caminhos exportados após confirmação visual e `cad_check.passed == true`.
